//! Result rows: one shared buffer per result set, decoded on access.

use crate::{
    Error, FromSql, Oid, Result,
    types::{Shape, shape},
    wire::Body,
};
use std::{fmt, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    name: Box<str>,
    ty: Oid,
    shape: Shape,
}

impl Column {
    pub(crate) fn new(name: &str, ty: Oid) -> Self {
        Self {
            name: name.into(),
            ty,
            shape: shape(ty),
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn type_oid(&self) -> Oid {
        self.ty
    }
}

const NULL: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Cell {
    start: u32,
    length: u32,
}

/// every row of one result, stored back to back
pub(crate) struct RowSet {
    columns: Arc<[Column]>,
    data: Vec<u8>,
    cells: Vec<Cell>,
    rows: usize,
}

impl RowSet {
    pub fn new(columns: Arc<[Column]>) -> Self {
        Self {
            columns,
            data: Vec::new(),
            cells: Vec::new(),
            rows: 0,
        }
    }

    /// checks one `DataRow` body against the columns and keeps it
    pub fn push(&mut self, body: &[u8]) -> Result<()> {
        let base = self.data.len();
        if base + body.len() >= NULL as usize {
            return Err(Error::Protocol("result larger than 4 GiB"));
        }
        let mut fields = Body::new(body);
        if usize::try_from(fields.i16()?) != Ok(self.columns.len()) {
            return Err(Error::Protocol("row width differs from its description"));
        }
        for column in self.columns.iter() {
            let length = fields.i32()?;
            if length == -1 {
                self.cells.push(Cell {
                    start: 0,
                    length: NULL,
                });
                continue;
            }
            let length =
                usize::try_from(length).map_err(|_| Error::Protocol("negative value length"))?;
            let start = base + body.len() - fields.remaining();
            let value = fields.take(length)?;
            let sound = match column.shape {
                Shape::Any => true,
                Shape::Fixed(width) => length == width as usize,
                Shape::Utf8 => std::str::from_utf8(value).is_ok(),
                Shape::Jsonb => value
                    .split_first()
                    .is_some_and(|(_, text)| std::str::from_utf8(text).is_ok()),
            };
            if !sound {
                return Err(Error::Protocol("value does not fit its column type"));
            }
            self.cells.push(Cell {
                start: start as u32,
                length: length as u32,
            });
        }
        fields.end()?;
        self.data.extend_from_slice(body);
        self.rows += 1;
        Ok(())
    }

    pub fn into_rows(self) -> Vec<Row> {
        let width = self.columns.len();
        let count = self.rows;
        let set = Arc::new(self);
        (0..count)
            .map(|row| Row {
                set: Arc::clone(&set),
                first: row * width,
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct Row {
    set: Arc<RowSet>,
    first: usize,
}

impl Row {
    pub fn columns(&self) -> &[Column] {
        &self.set.columns
    }
    pub fn len(&self) -> usize {
        self.set.columns.len()
    }
    pub fn is_empty(&self) -> bool {
        self.set.columns.is_empty()
    }

    /// reads a column, by position or by name
    ///
    /// # Panics
    ///
    /// Panics if the column does not exist or cannot be read as `T`; both
    /// are mistakes in the calling code, not conditions of the data.
    #[track_caller]
    pub fn get<'a, I: RowIndex, T: FromSql<'a>>(&'a self, index: I) -> T {
        match self.try_get(index) {
            Ok(value) => value,
            Err(error) => panic!("error retrieving column: {error}"),
        }
    }

    pub fn try_get<'a, I: RowIndex, T: FromSql<'a>>(&'a self, index: I) -> Result<T> {
        let Some(index) = index.position(&self.set.columns) else {
            return Err(Error::NoColumn(index.to_string()));
        };
        let ty = self.set.columns[index].ty;
        if !T::accepts(ty) {
            return Err(Error::Column {
                index,
                ty,
                reason: format!("cannot be read as {}", std::any::type_name::<T>()),
            });
        }
        let cell = self.set.cells[self.first + index];
        let value = if cell.length == NULL {
            T::from_null(ty)
        } else {
            let start = cell.start as usize;
            T::from_sql(ty, &self.set.data[start..start + cell.length as usize])
        };
        value.map_err(|reason| Error::Column { index, ty, reason })
    }
}

impl fmt::Debug for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Row")
            .field("columns", &self.columns())
            .finish_non_exhaustive()
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for usize {}
    impl Sealed for &str {}
}

/// how a column is named in [`Row::get`]: its position or its name
pub trait RowIndex: sealed::Sealed + fmt::Display {
    #[doc(hidden)]
    fn position(&self, columns: &[Column]) -> Option<usize>;
}

impl RowIndex for usize {
    fn position(&self, columns: &[Column]) -> Option<usize> {
        (*self < columns.len()).then_some(*self)
    }
}

impl RowIndex for &str {
    fn position(&self, columns: &[Column]) -> Option<usize> {
        columns.iter().position(|column| &*column.name == *self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oid;

    fn set() -> RowSet {
        RowSet::new(Arc::from(vec![
            Column::new("id", oid::INT8),
            Column::new("name", oid::TEXT),
            Column::new("doc", oid::JSONB),
            Column::new("data", oid::BYTEA),
        ]))
    }

    fn row(values: &[Option<&[u8]>]) -> Vec<u8> {
        let mut body = (values.len() as i16).to_be_bytes().to_vec();
        for value in values {
            match value {
                Some(value) => {
                    body.extend_from_slice(&(value.len() as i32).to_be_bytes());
                    body.extend_from_slice(value);
                }
                None => body.extend_from_slice(&(-1i32).to_be_bytes()),
            }
        }
        body
    }

    #[test]
    fn rows_decode_from_the_shared_buffer() {
        let mut set = set();
        let first = [
            Some(&7i64.to_be_bytes()[..]),
            Some(b"ab"),
            Some(b"\x01{}"),
            None,
        ];
        let second = [
            Some(&9i64.to_be_bytes()[..]),
            None,
            None,
            Some(&[1u8, 2, 3][..]),
        ];
        set.push(&row(&first)).unwrap();
        set.push(&row(&second)).unwrap();
        let rows = set.into_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<_, i64>(0), 7);
        assert_eq!(rows[0].get::<_, &str>("name"), "ab");
        assert_eq!(rows[0].get::<_, Option<&[u8]>>(3), None);
        assert_eq!(rows[1].get::<_, i64>("id"), 9);
        assert_eq!(rows[1].get::<_, Option<String>>(1), None);
        assert_eq!(rows[1].get::<_, Vec<u8>>(3), [1, 2, 3]);
        assert!(matches!(
            rows[0].try_get::<_, i32>(0),
            Err(Error::Column {
                index: 0,
                ty: oid::INT8,
                ..
            })
        ));
        assert!(matches!(
            rows[1].try_get::<_, String>(1),
            Err(Error::Column { index: 1, .. })
        ));
        assert!(matches!(
            rows[0].try_get::<_, i64>(4),
            Err(Error::NoColumn(_))
        ));
        assert!(matches!(
            rows[0].try_get::<_, i64>("nope"),
            Err(Error::NoColumn(_))
        ));
    }

    #[test]
    fn malformed_rows_are_refused() {
        let id = 7i64.to_be_bytes();
        let good = row(&[Some(&id[..]), Some(b"ab"), Some(b"\x01{}"), None]);
        set().push(&good).unwrap();
        for cut in 0..good.len() {
            assert!(set().push(&good[..cut]).is_err(), "truncated at {cut}");
        }
        let mut long = good.clone();
        long.push(0);
        assert!(set().push(&long).is_err());
        let narrow = row(&[Some(&id[..]), Some(b"ab"), Some(b"\x01{}")]);
        assert!(set().push(&narrow).is_err());
        let short_int = row(&[Some(&id[..4]), Some(b"ab"), Some(b"\x01{}"), None]);
        assert!(set().push(&short_int).is_err());
        let bad_text = row(&[Some(&id[..]), Some(b"\xff"), Some(b"\x01{}"), None]);
        assert!(set().push(&bad_text).is_err());
        let empty_jsonb = row(&[Some(&id[..]), Some(b"ab"), Some(b""), None]);
        assert!(set().push(&empty_jsonb).is_err());
        let mut negative = good.clone();
        negative[2..6].copy_from_slice(&(-2i32).to_be_bytes());
        assert!(set().push(&negative).is_err());
        let mut huge = good;
        huge[2..6].copy_from_slice(&i32::MAX.to_be_bytes());
        assert!(set().push(&huge).is_err());
    }
}
