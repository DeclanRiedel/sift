use crate::{error::Error, sql_read_bytes::SqlReadBytes, tds::Numeric, ColumnData};

pub(crate) async fn decode<R>(src: &mut R, len: u8) -> crate::Result<ColumnData<'static>>
where
    R: SqlReadBytes + Unpin,
{
    let res = match len {
        0 => ColumnData::Numeric(None),
        4 => ColumnData::Numeric(Some(Numeric::new_with_scale(src.read_i32_le().await? as i128, 4))),
        8 => ColumnData::Numeric(Some(Numeric::new_with_scale({
            let high = src.read_i32_le().await? as i64;
            let low = src.read_u32_le().await? as i64;
            ((high << 32) | low) as i128
        }, 4))),
        _ => {
            return Err(Error::Protocol(
                format!("money: length of {} is invalid", len).into(),
            ))
        }
    };

    Ok(res)
}
