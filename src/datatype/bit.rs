use pgrx::pgrx_sql_entity_graph::metadata::{
    ArgumentError, Returns, ReturnsError, SqlMapping, SqlTranslatable,
};
use pgrx::{pg_sys, FromDatum, IntoDatum, PgMemoryContexts};
use std::mem;

const BITS_PER_BYTE: usize = 8;
const PREFIX_BYTES: usize = 4;

#[repr(C)]
#[derive(Debug, Default)]
pub struct BitRaw {
    pub vl_len_: pg_sys::int32,
    pub bit_len: pg_sys::int32,
    pub bit_dat: pg_sys::__IncompleteArrayField<pg_sys::bits8>,
}

pub struct Bit {
    pub data: Vec<bool>,
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
            let ptr = datum.cast_mut_ptr::<BitRaw>();
            let varlena = unsafe { pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
            unsafe {
                pgrx::warning!(
                    "1b {:?} 4b {:?}",
                    pgrx::varatt_is_1b(varlena),
                    pgrx::varatt_is_4b(varlena)
                )
            };
            let bit_len = unsafe { (*ptr).bit_len } as usize;
            let slice = unsafe { pgrx::varlena_to_byte_slice(varlena) };
            pgrx::warning!("{:?}", slice);

            let mut data = Vec::new();
            for i in 0..bit_len {
                let byte_no: usize = i / BITS_PER_BYTE;
                let bit_no: usize = BITS_PER_BYTE - 1 - (i % BITS_PER_BYTE);
                let left = slice[byte_no + PREFIX_BYTES];
                let right = 1 << bit_no as u8;
                data.push(left & right != 0);
            }
            Some(Bit { data })
        }
    }
}

impl IntoDatum for Bit {
    fn into_datum(mut self) -> Option<pg_sys::Datum> {
        let bit_len = self.data.len();
        let byte_len = bit_len.div_ceil(BITS_PER_BYTE);
        pgrx::warning!("{:?} {:?}", bit_len, byte_len);
        let mut bytes = vec![0u8; byte_len];

        let mut len_byte_no = bit_len;
        // for i in 0..PREFIX_BYTES {
        //     let next_byte_no = len_byte_no / 256;
        //     let value = (len_byte_no % 256) as u8;
        //     bytes[i] = value;
        //     len_byte_no = next_byte_no;
        // }
        for i in 0..bit_len {
            let byte_no: usize = i / BITS_PER_BYTE;
            let bit_no: usize = BITS_PER_BYTE - 1 - (i % BITS_PER_BYTE);
            let value = (self.data[i] as u8) << bit_no;
            bytes[byte_no] |= value;
        }
        pgrx::warning!("{:?}", bytes);
        pgrx::warning!("1");

        // let varlena = unsafe { pg_sys::palloc(byte_len) } as *mut pg_sys::varlena;
        pgrx::warning!("2");
        // let varattrib_4b: *mut _ = unsafe {
        //     &mut varlena
        //         .cast::<pg_sys::varattrib_4b>()
        //         .as_mut()
        //         .unwrap_unchecked()
        //         .va_4byte
        // };
        let data = unsafe { pg_sys::palloc(mem::size_of::<BitRaw>()) } as *mut BitRaw;
        pgrx::warning!("3");
        debug_assert!(byte_len < (u32::MAX as usize >> 2));
        pgrx::warning!("4");
        unsafe { std::ptr::addr_of_mut!((*data).bit_len).write(bit_len as i32) };
        // unsafe {
        //     std::ptr::copy_nonoverlapping(
        //         bytes.as_mut_ptr(),
        //         std::ptr::addr_of_mut!((*data).bit_dat).cast::<pg_sys::bits8>(),
        //         byte_len,
        //     )
        // };
        // unsafe {
        //     std::ptr::copy(
        //         bytes.as_mut_ptr(),
        //         std::ptr::addr_of_mut!((*data).bit_dat).cast::<pg_sys::bits8>(),
        //         byte_len,
        //     )
        // };
        unsafe {
            std::ptr::copy(
                bytes.as_mut_ptr(),
                std::ptr::addr_of_mut!((*data).bit_dat).cast::<pg_sys::bits8>(),
                byte_len,
            )
        };
        unsafe {
            pgrx::set_varsize_4b(
                data as *mut pg_sys::varlena,
                (byte_len + 2 * PREFIX_BYTES) as i32,
            )
        };
        pgrx::warning!("5");

        pgrx::warning!("6");
        pgrx::warning!("7");

        let datum = pg_sys::Datum::from(data);
        let ptr = datum.cast_mut_ptr::<BitRaw>();
        let varlena = unsafe { pg_sys::pg_detoast_datum(datum.cast_mut_ptr()) };
        unsafe {
            pgrx::warning!(
                "1b {:?} 4b {:?}",
                pgrx::varatt_is_1b(varlena),
                pgrx::varatt_is_4b(varlena)
            )
        };
        let bit_len = unsafe { (*ptr).bit_len } as usize;
        let slice = unsafe { pgrx::varlena_to_byte_slice(varlena) };
        pgrx::warning!("{:?}", slice);
        panic!("123");
        Some(pg_sys::Datum::from(data))
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
