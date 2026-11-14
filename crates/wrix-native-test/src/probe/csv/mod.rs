use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// probe CSV exceeds its size or shape bound
    Oversized,
    /// probe CSV is not UTF-8
    Encoding,
    /// probe CSV has an invalid quoted field or record
    Syntax,
    /// probe CSV has missing, empty or duplicate column names
    Header,
    /// probe CSV does not contain the required column: {0}
    Column(&'static str),
}

pub struct Table {
    columns: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

#[derive(Clone, Copy)]
enum State {
    Start,
    Plain,
    Quoted,
    Closed,
}

struct Reader {
    state: State,
    field: Vec<u8>,
    row: Vec<String>,
    records: Vec<Vec<String>>,
    pending: bool,
}
impl Reader {
    fn field(&mut self) -> Result<(), Error> {
        if self.row.len() >= 256 {
            return Err(Error::Oversized);
        }
        self.row
            .push(String::from_utf8(std::mem::take(&mut self.field)).map_err(|_| Error::Encoding)?);
        self.state = State::Start;
        Ok(())
    }
    fn record(&mut self) -> Result<(), Error> {
        if self.records.len() >= 10_001 {
            return Err(Error::Oversized);
        }
        self.field()?;
        self.records.push(std::mem::take(&mut self.row));
        self.pending = false;
        Ok(())
    }
    fn byte(&mut self, byte: u8) -> Result<(), Error> {
        match (self.state, byte) {
            (State::Start, b'"') => self.state = State::Quoted,
            (State::Quoted, b'"') => self.state = State::Closed,
            (State::Closed, b'"') => {
                self.field.push(b'"');
                self.state = State::Quoted;
            }
            (State::Quoted, byte) => self.field.push(byte),
            (_, b',') => self.field()?,
            (_, b'\n') => return self.record(),
            (State::Closed, _) | (State::Plain, b'"') => return Err(Error::Syntax),
            (_, byte) => {
                self.state = State::Plain;
                self.field.push(byte);
            }
        }
        self.pending = true;
        if self.field.len() > 65_536 {
            return Err(Error::Oversized);
        }
        Ok(())
    }
}

impl Table {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > 1024 * 1024 {
            return Err(Error::Oversized);
        }
        let mut reader = Reader {
            state: State::Start,
            field: Vec::new(),
            row: Vec::new(),
            records: Vec::new(),
            pending: false,
        };
        let mut bytes = bytes.iter().copied();
        while let Some(byte) = bytes.next() {
            if byte == b'\r' && !matches!(reader.state, State::Quoted) {
                if bytes.next() != Some(b'\n') {
                    return Err(Error::Syntax);
                }
                reader.byte(b'\n')?;
            } else {
                reader.byte(byte)?;
            }
        }
        if matches!(reader.state, State::Quoted) {
            return Err(Error::Syntax);
        }
        if reader.pending {
            reader.record()?;
        }
        let mut records = reader.records.into_iter();
        let header = records.next().ok_or(Error::Header)?;
        let mut columns = BTreeMap::new();
        for (index, name) in header.into_iter().enumerate() {
            if name.is_empty() || columns.insert(name, index).is_some() {
                return Err(Error::Header);
            }
        }
        let rows: Vec<_> = records.collect();
        if rows.iter().any(|row| row.len() != columns.len()) {
            return Err(Error::Syntax);
        }
        Ok(Self { columns, rows })
    }
    pub fn column(&self, name: &'static str) -> Result<Vec<&str>, Error> {
        let index = self.columns.get(name).ok_or(Error::Column(name))?;
        self.rows
            .iter()
            .map(|row| row.get(*index).map(String::as_str).ok_or(Error::Syntax))
            .collect()
    }
}

#[cfg(test)]
mod test;
