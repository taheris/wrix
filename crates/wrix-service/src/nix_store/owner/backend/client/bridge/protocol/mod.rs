//! Version-one framing has finite memory and rejects unknown operations before payload allocation.
use super::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub(super) const CHUNK: usize = 16_384;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    Open,
    Data,
    Close,
    Credit,
}
impl Kind {
    const fn wire(self) -> u8 {
        match self {
            Self::Open => 1,
            Self::Data => 2,
            Self::Close => 3,
            Self::Credit => 4,
        }
    }
}
#[derive(Debug)]
pub(super) struct Frame {
    pub kind: Kind,
    pub id: u32,
    pub data: Vec<u8>,
}
impl Frame {
    pub const fn empty(kind: Kind, id: u32) -> Self {
        Self {
            kind,
            id,
            data: Vec::new(),
        }
    }
    fn header(header: [u8; 9]) -> Result<(Kind, u32, usize), Error> {
        let kind = match header[0] {
            1 => Kind::Open,
            2 => Kind::Data,
            3 => Kind::Close,
            4 => Kind::Credit,
            _ => return Err(Error::Invalid),
        };
        let id = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let length = u32::from_be_bytes([header[5], header[6], header[7], header[8]]) as usize;
        if id == 0 || length > CHUNK || (kind == Kind::Data) != (length != 0) {
            return Err(Error::Invalid);
        }
        Ok((kind, id, length))
    }
    pub async fn read(input: &mut (impl AsyncRead + Unpin)) -> Result<Self, Error> {
        let mut header = [0; 9];
        input.read_exact(&mut header).await?;
        let (kind, id, length) = Self::header(header)?;
        let mut data = vec![0; length];
        input.read_exact(&mut data).await?;
        Ok(Self { kind, id, data })
    }
    pub async fn write(&self, output: &mut (impl AsyncWrite + Unpin)) -> Result<(), Error> {
        let mut header = [0; 9];
        header[0] = self.kind.wire();
        header[1..5].copy_from_slice(&self.id.to_be_bytes());
        header[5..].copy_from_slice(
            &u32::try_from(self.data.len())
                .map_err(|_| Error::Invalid)?
                .to_be_bytes(),
        );
        Self::header(header)?;
        output.write_all(&header).await?;
        output.write_all(&self.data).await?;
        Ok(())
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn frames_reject_unknown_zero_oversize_and_control_payloads() {
        for (kind, id, length) in [
            (0, 1, 0),
            (1, 0, 0),
            (2, 1, 0),
            (2, 1, CHUNK + 1),
            (1, 1, 1),
            (3, 1, 1),
            (4, 1, 1),
        ] {
            let mut header = [0; 9];
            header[0] = kind;
            header[1..5].copy_from_slice(&u32::try_from(id).unwrap().to_be_bytes());
            header[5..].copy_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
            assert!(Frame::header(header).is_err());
        }
    }
    #[tokio::test]
    async fn fragmented_frames_roundtrip_without_interpreting_ssh_payload() {
        let (mut tx, mut rx) = tokio::io::duplex(7);
        let writer = tokio::spawn(async move {
            Frame {
                kind: Kind::Data,
                id: 5,
                data: b"SSH-2.0-untrusted\0payload".to_vec(),
            }
            .write(&mut tx)
            .await
            .unwrap();
        });
        let frame = Frame::read(&mut rx).await.unwrap();
        assert_eq!(frame.id, 5);
        assert_eq!(frame.kind, Kind::Data);
        assert_eq!(frame.data, b"SSH-2.0-untrusted\0payload");
        writer.await.unwrap();
    }
}
