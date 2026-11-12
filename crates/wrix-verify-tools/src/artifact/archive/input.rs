//! Validated gzip transport with restart points at complete member boundaries.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub const MAX_GZIP_CHECKPOINTS: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub compressed: u64,
    pub uncompressed: u64,
}

enum GzipState<R> {
    Member(flate2::bufread::GzDecoder<R>),
    Complete,
    Failed,
}

pub(super) struct Gzip<R> {
    state: GzipState<R>,
    position: u64,
    checkpoints: Vec<Checkpoint>,
}

impl<R: BufRead + Seek> Gzip<R> {
    pub(super) fn new(mut reader: R, position: u64) -> io::Result<Self> {
        let compressed = reader.stream_position()?;
        Ok(Self {
            state: GzipState::Member(flate2::bufread::GzDecoder::new(reader)),
            position,
            checkpoints: vec![Checkpoint {
                compressed,
                uncompressed: position,
            }],
        })
    }
}

impl<R: BufRead + Seek> Read for Gzip<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            match std::mem::replace(&mut self.state, GzipState::Failed) {
                GzipState::Complete => {
                    self.state = GzipState::Complete;
                    return Ok(0);
                }
                GzipState::Failed => {
                    return Err(io::Error::other("gzip transport previously failed"));
                }
                GzipState::Member(mut decoder) => {
                    let count = decoder.read(bytes)?;
                    if count != 0 {
                        self.position = self
                            .position
                            .checked_add(u64::try_from(count).map_err(io::Error::other)?)
                            .ok_or_else(|| io::Error::other("gzip offset overflow"))?;
                        self.state = GzipState::Member(decoder);
                        return Ok(count);
                    }
                    let mut reader = decoder.into_inner();
                    loop {
                        let buffered = reader.fill_buf()?;
                        if buffered.is_empty() {
                            self.state = GzipState::Complete;
                            return Ok(0);
                        }
                        let zeros = buffered.iter().take_while(|byte| **byte == 0).count();
                        reader.consume(zeros);
                        if zeros == 0 {
                            break;
                        }
                    }
                    if self.checkpoints.len() < MAX_GZIP_CHECKPOINTS {
                        self.checkpoints.push(Checkpoint {
                            compressed: reader.stream_position()?,
                            uncompressed: self.position,
                        });
                    }
                    self.state = GzipState::Member(flate2::bufread::GzDecoder::new(reader));
                }
            }
        }
    }
}

enum Transport {
    Plain(BufReader<File>),
    Gzip(Gzip<BufReader<File>>),
}

pub struct Input(Transport);

impl Input {
    pub fn checkpoints(&self) -> &[Checkpoint] {
        match &self.0 {
            Transport::Plain(_) => &[],
            Transport::Gzip(reader) => &reader.checkpoints,
        }
    }
}

impl Read for Input {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match &mut self.0 {
            Transport::Plain(reader) => reader.read(bytes),
            Transport::Gzip(reader) => reader.read(bytes),
        }
    }
}

#[cfg(test)]
mod test;

pub fn open(path: &Path) -> io::Result<Input> {
    open_at(path, 0, &[])
}

pub fn open_at(path: &Path, offset: u64, checkpoints: &[Checkpoint]) -> io::Result<Input> {
    let mut source = BufReader::new(File::open(path)?);
    if source.fill_buf()?.starts_with(&[0x1f, 0x8b]) {
        let checkpoint = checkpoints
            .iter()
            .rev()
            .find(|point| point.uncompressed <= offset)
            .copied()
            .unwrap_or(Checkpoint {
                compressed: 0,
                uncompressed: 0,
            });
        if checkpoint.compressed >= source.get_ref().metadata()?.len() {
            return Err(io::Error::other("gzip restart point exceeds archive"));
        }
        source.seek(SeekFrom::Start(checkpoint.compressed))?;
        let mut reader = Input(Transport::Gzip(Gzip::new(source, checkpoint.uncompressed)?));
        let skip = offset - checkpoint.uncompressed;
        if super::drain(&mut (&mut reader).take(skip))? != skip {
            return Err(io::Error::other("truncated archive before member"));
        }
        Ok(reader)
    } else {
        if offset > source.get_ref().metadata()?.len() {
            return Err(io::Error::other("truncated archive before member"));
        }
        source.seek(SeekFrom::Start(offset))?;
        Ok(Input(Transport::Plain(source)))
    }
}
