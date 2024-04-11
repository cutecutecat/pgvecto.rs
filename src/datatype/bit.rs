use pgrx::pgrx_sql_entity_graph::metadata::{
    ArgumentError, Returns, ReturnsError, SqlMapping, SqlTranslatable,
};
use pgrx::{pg_sys, FromDatum, IntoDatum, PgMemoryContexts};
use std::mem;

const BITS_PER_BYTE: usize = 8;

#[repr(C)]
#[derive(Debug, Default)]
pub struct Bit {
    pub vl_len_: pg_sys::int32,
    pub bit_len: pg_sys::int32,
    pub bit_dat: pg_sys::__IncompleteArrayField<pg_sys::bits8>,
}

impl Bit {
    pub fn varbits(&self) -> Vec<bool> {
        let mut vec = Vec::new();
        for n in 0..self.len() {
            let byte_no: usize = n / BITS_PER_BYTE;
            let bit_no: usize = BITS_PER_BYTE - 1 - (n % BITS_PER_BYTE);
            let left = unsafe { *self.bit_dat.as_ptr().add(byte_no) };
            let right = 1 << bit_no as u8;
            pgrx::warning!(
                "byte_no {:?} bit_no {:?} left {:?} right {:?}",
                byte_no,
                bit_no,
                left,
                right
            );
            vec.push(left & right != 0);
        }
        vec
    }
    pub fn len(&self) -> usize {
        self.bit_len as usize
    }
}

impl FromDatum for Bit {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: pg_sys::Oid,
    ) -> Option<Self> {
        if is_null {
            None
        } else {
            let ptr = datum.cast_mut_ptr::<Bit>();
            unsafe { Some(ptr.read()) }
        }
    }
}

impl IntoDatum for Bit {
    fn into_datum(mut self) -> Option<pg_sys::Datum> {
        let ptr = unsafe {
            PgMemoryContexts::CurrentMemoryContext.copy_ptr_into(&mut self, mem::size_of::<Bit>())
        };
        Some(ptr.into())
    }

    fn type_oid() -> pg_sys::Oid {
        pg_sys::BITOID
    }
}

unsafe impl SqlTranslatable for Bit {
    fn argument_sql() -> Result<SqlMapping, ArgumentError> {
        Ok(SqlMapping::literal("bit"))
    }

    fn return_sql() -> Result<Returns, ReturnsError> {
        Ok(Returns::One(SqlMapping::literal("bit")))
    }
}
