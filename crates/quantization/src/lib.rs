#![feature(avx512_target_feature)]
#![cfg_attr(target_arch = "x86_64", feature(stdarch_x86_avx512))]
#![allow(clippy::identity_op)]
#![allow(clippy::needless_range_loop)]

pub mod fast_scan;
pub mod operator;
pub mod product;
pub mod rabitq;
pub mod reranker;
pub mod scalar;
pub mod trivial;
mod utils;

use self::product::ProductQuantizer;
use self::rabitq::RaBitQuantizer;
use self::scalar::ScalarQuantizer;
use crate::operator::OperatorQuantization;
use base::index::*;
use base::operator::*;
use base::scalar::*;
use base::search::*;
use base::vector::*;
use common::json::Json;
use common::mmap_array::MmapArray;
use serde::Deserialize;
use serde::Serialize;
use std::cmp::Reverse;
use std::ops::Range;
use std::path::Path;
use trivial::TrivialQuantizer;
use utils::InfiniteByteChunks;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Quantizer<O: OperatorQuantization> {
    Trivial(TrivialQuantizer<O>),
    Scalar(ScalarQuantizer<O>),
    Product(ProductQuantizer<O>),
    RaBitQ(RaBitQuantizer<O>),
}

impl<O: OperatorQuantization> Quantizer<O> {
    pub fn train(
        vector_options: VectorOptions,
        quantization_options: QuantizationOptions,
        vectors: &impl Vectors<O>,
        transform: impl Fn(Borrowed<'_, O>) -> Owned<O> + Copy + Send + Sync,
    ) -> Self {
        use QuantizationOptions::*;
        match quantization_options {
            Trivial(trivial_quantization_options) => Self::Trivial(TrivialQuantizer::train(
                vector_options,
                trivial_quantization_options,
                vectors,
                transform,
            )),
            Scalar(scalar_quantization_options) => Self::Scalar(ScalarQuantizer::train(
                vector_options,
                scalar_quantization_options,
                vectors,
                transform,
            )),
            Product(product_quantization_options) => Self::Product(ProductQuantizer::train(
                vector_options,
                product_quantization_options,
                vectors,
                transform,
            )),
            RaBitQ(rabitq_quantization_options) => Self::RaBitQ(RaBitQuantizer::train(
                vector_options,
                rabitq_quantization_options,
                vectors,
                transform,
            )),
        }
    }
}

pub enum QuantizationPreprocessed<O: OperatorQuantization> {
    Trivial(O::TrivialQuantizationPreprocessed),
    RaBit(O::RabitQuantizationPreprocessed),
    Scalar(O::QuantizationPreprocessed),
    Product(O::QuantizationPreprocessed),
}

pub struct Quantization<O: OperatorQuantization> {
    train: Json<Quantizer<O>>,
    codes: MmapArray<u8>,
    packed_codes: MmapArray<u8>,
}

impl<O: OperatorQuantization> Quantization<O> {
    pub fn create(
        path: impl AsRef<Path>,
        vector_options: VectorOptions,
        quantization_options: QuantizationOptions,
        vectors: &impl Vectors<O>,
        transform: impl Fn(Borrowed<'_, O>) -> Owned<O> + Copy + Send + Sync,
    ) -> Self {
        use Quantizer::*;
        std::fs::create_dir(path.as_ref()).unwrap();
        let train = Quantizer::train(vector_options, quantization_options, vectors, transform);
        let train = Json::create(path.as_ref().join("train"), train);
        let codes = MmapArray::create(path.as_ref().join("codes"), {
            fn merge_8([b0, b1, b2, b3, b4, b5, b6, b7]: [u8; 8]) -> u8 {
                b0 | (b1 << 1)
                    | (b2 << 2)
                    | (b3 << 3)
                    | (b4 << 4)
                    | (b5 << 5)
                    | (b6 << 6)
                    | (b7 << 7)
            }
            fn merge_4([b0, b1, b2, b3]: [u8; 4]) -> u8 {
                b0 | (b1 << 2) | (b2 << 4) | (b3 << 6)
            }
            fn merge_2([b0, b1]: [u8; 2]) -> u8 {
                b0 | (b1 << 4)
            }
            match &*train {
                Trivial(_) => Box::new(std::iter::empty()) as Box<dyn Iterator<Item = u8>>,
                Scalar(x) => Box::new((0..vectors.len()).flat_map(|i| {
                    let vector = vectors.vector(i).to_vec();
                    let codes = x.encode(&vector);
                    let bytes = x.bytes();
                    match x.bits() {
                        1 => InfiniteByteChunks::new(codes.into_iter())
                            .map(merge_8)
                            .take(bytes as usize)
                            .collect(),
                        2 => InfiniteByteChunks::new(codes.into_iter())
                            .map(merge_4)
                            .take(bytes as usize)
                            .collect(),
                        4 => InfiniteByteChunks::new(codes.into_iter())
                            .map(merge_2)
                            .take(bytes as usize)
                            .collect(),
                        8 => codes,
                        _ => unreachable!(),
                    }
                })),
                Product(x) => Box::new((0..vectors.len()).flat_map(|i| {
                    let vector = vectors.vector(i).to_vec();
                    let codes = x.encode(&vector);
                    let bytes = x.bytes();
                    match x.bits() {
                        1 => InfiniteByteChunks::new(codes.into_iter())
                            .map(merge_8)
                            .take(bytes as usize)
                            .collect(),
                        2 => InfiniteByteChunks::new(codes.into_iter())
                            .map(merge_4)
                            .take(bytes as usize)
                            .collect(),
                        4 => InfiniteByteChunks::new(codes.into_iter())
                            .map(merge_2)
                            .take(bytes as usize)
                            .collect(),
                        8 => codes,
                        _ => unreachable!(),
                    }
                })),
                RaBitQ(_) => Box::new(std::iter::empty()) as Box<dyn Iterator<Item = u8>>,
            }
        });
        let packed_codes = MmapArray::create(
            path.as_ref().join("packed_codes"),
            match &*train {
                Trivial(_) => Box::new(std::iter::empty()) as Box<dyn Iterator<Item = u8>>,
                Scalar(x) => match x.bits() {
                    4 => {
                        use fast_scan::b4::{pack, BLOCK_SIZE};
                        let blocks = vectors.len().div_ceil(BLOCK_SIZE);
                        Box::new((0..blocks).flat_map(|block| {
                            let width = x.width();
                            let n = vectors.len();
                            let raw = std::array::from_fn::<_, { BLOCK_SIZE as _ }, _>(|i| {
                                let id = BLOCK_SIZE * block + i as u32;
                                x.encode(&vectors.vector(std::cmp::min(id, n - 1)).to_vec())
                            });
                            pack(width, raw)
                        })) as Box<dyn Iterator<Item = u8>>
                    }
                    _ => Box::new(std::iter::empty()) as Box<dyn Iterator<Item = u8>>,
                },
                Product(x) => match x.bits() {
                    4 => {
                        use fast_scan::b4::{pack, BLOCK_SIZE};
                        let blocks = vectors.len().div_ceil(BLOCK_SIZE);
                        Box::new((0..blocks).flat_map(|block| {
                            let width = x.width();
                            let n = vectors.len();
                            let raw = std::array::from_fn::<_, { BLOCK_SIZE as _ }, _>(|i| {
                                let id = BLOCK_SIZE * block + i as u32;
                                x.encode(&vectors.vector(std::cmp::min(id, n - 1)).to_vec())
                            });
                            pack(width, raw)
                        })) as Box<dyn Iterator<Item = u8>>
                    }
                    _ => Box::new(std::iter::empty()) as Box<dyn Iterator<Item = u8>>,
                },
                RaBitQ(_x) => Box::new(std::iter::empty()) as Box<dyn Iterator<Item = u8>>,
            },
        );
        Self {
            train,
            codes,
            packed_codes,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Self {
        let train = Json::open(path.as_ref().join("train"));
        let codes = MmapArray::open(path.as_ref().join("codes"));
        let packed_codes = MmapArray::open(path.as_ref().join("packed_codes"));
        Self {
            train,
            codes,
            packed_codes,
        }
    }

    pub fn preprocess(&self, lhs: Borrowed<'_, O>) -> QuantizationPreprocessed<O> {
        match &*self.train {
            Quantizer::Trivial(x) => QuantizationPreprocessed::Trivial(x.preprocess(lhs)),
            Quantizer::Scalar(x) => QuantizationPreprocessed::Scalar(x.preprocess(lhs)),
            Quantizer::Product(x) => QuantizationPreprocessed::Product(x.preprocess(lhs)),
            _ => unreachable!(),
        }
    }

    pub fn project(&self, lhs: &[Scalar<O>]) -> Vec<Scalar<O>> {
        match &*self.train {
            Quantizer::RaBitQ(x) => x.project(lhs),
            _ => unreachable!(),
        }
    }

    pub fn projection_preprocess(
        &self,
        lhs: Borrowed<'_, O>,
        distance: F32,
    ) -> QuantizationPreprocessed<O> {
        match &*self.train {
            Quantizer::RaBitQ(x) => QuantizationPreprocessed::RaBit(x.preprocess(lhs, distance)),
            _ => unreachable!(),
        }
    }

    pub fn process(
        &self,
        vectors: &impl Vectors<O>,
        preprocessed: &QuantizationPreprocessed<O>,
        u: u32,
    ) -> F32 {
        match (&*self.train, preprocessed) {
            (Quantizer::Trivial(x), QuantizationPreprocessed::Trivial(lhs)) => {
                let rhs = vectors.vector(u);
                x.process(lhs, rhs)
            }
            (Quantizer::Scalar(x), QuantizationPreprocessed::Scalar(lhs)) => {
                let bytes = x.bytes() as usize;
                let start = u as usize * bytes;
                let end = start + bytes;
                let rhs = &self.codes[start..end];
                x.process(lhs, rhs)
            }
            (Quantizer::Product(x), QuantizationPreprocessed::Product(lhs)) => {
                let bytes = x.bytes() as usize;
                let start = u as usize * bytes;
                let end = start + bytes;
                let rhs = &self.codes[start..end];
                x.process(lhs, rhs)
            }
            (Quantizer::RaBitQ(x), QuantizationPreprocessed::RaBit(lhs)) => {
                x.process(lhs, u as usize)
            }
            _ => unreachable!(),
        }
    }

    pub fn push_batch(
        &self,
        preprocessed: &QuantizationPreprocessed<O>,
        rhs: Range<u32>,
        heap: &mut Vec<(Reverse<F32>, u32)>,
        sq_fast_scan: bool,
        pq_fast_scan: bool,
    ) {
        match (&*self.train, preprocessed) {
            (Quantizer::Trivial(x), QuantizationPreprocessed::Trivial(lhs)) => {
                x.push_batch(lhs, rhs, heap)
            }
            (Quantizer::Scalar(x), QuantizationPreprocessed::Scalar(lhs)) => x.push_batch(
                lhs,
                rhs,
                heap,
                &self.codes,
                &self.packed_codes,
                sq_fast_scan,
            ),
            (Quantizer::Product(x), QuantizationPreprocessed::Product(lhs)) => x.push_batch(
                lhs,
                rhs,
                heap,
                &self.codes,
                &self.packed_codes,
                pq_fast_scan,
            ),
            _ => unreachable!(),
        }
    }

    pub fn flat_rerank<'a, T: 'a>(
        &'a self,
        heap: Vec<(Reverse<F32>, u32)>,
        r: impl Fn(u32) -> (F32, T) + 'a,
        sq_rerank_size: u32,
        pq_rerank_size: u32,
    ) -> Box<dyn RerankerPop<T> + 'a> {
        use Quantizer::*;
        match &*self.train {
            Trivial(x) => Box::new(x.flat_rerank(heap, r)),
            Scalar(x) => Box::new(x.flat_rerank(heap, r, sq_rerank_size)),
            Product(x) => Box::new(x.flat_rerank(heap, r, pq_rerank_size)),
            _ => unreachable!(),
        }
    }

    pub fn ivf_projection_rerank<'a, T: 'a>(
        &'a self,
        rough_distances: Vec<(F32, u32)>,
        r: impl Fn(u32) -> (F32, T) + 'a,
    ) -> Box<dyn RerankerPop<T> + 'a> {
        use Quantizer::*;
        match &*self.train {
            RaBitQ(x) => Box::new(x.ivf_projection_rerank(rough_distances, r)),
            _ => unreachable!(),
        }
    }

    pub fn graph_rerank<'a, T: 'a>(
        &'a self,
        vector: Borrowed<'a, O>,
        r: impl Fn(u32) -> (F32, T) + 'a,
    ) -> Box<dyn GraphReranker<T> + 'a> {
        use Quantizer::*;
        match &*self.train {
            Trivial(x) => Box::new(x.graph_rerank(vector, r)),
            Scalar(x) => Box::new(x.graph_rerank(
                vector,
                |u| {
                    let bytes = x.bytes() as usize;
                    let start = u as usize * bytes;
                    let end = start + bytes;
                    &self.codes[start..end]
                },
                r,
            )),
            Product(x) => Box::new(x.graph_rerank(
                vector,
                |u| {
                    let bytes = x.bytes() as usize;
                    let start = u as usize * bytes;
                    let end = start + bytes;
                    &self.codes[start..end]
                },
                r,
            )),
            _ => unreachable!(),
        }
    }
}
