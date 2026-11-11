//! A whole repository as one pack, framed for Git's side band and sent while it is
//! read from the segment files: nothing the size of the pack is held in memory, and
//! the first bytes leave as soon as the request is understood.

use alt_pack_segment::Piece;
use bytes::{BufMut, Bytes, BytesMut};
use hyper::body::{Body, Frame};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

/// the most pack bytes one side-band packet carries
const PACKET: usize = 65515;

enum Stage {
    /// the packet that announces the pack
    Announce,
    /// inside piece `index`, at byte `at`
    Pieces {
        index: usize,
        at: usize,
    },
    /// the checksum over everything sent, when the pieces do not end with one
    Checksum,
    Flush,
    Done,
}

pub(super) struct PackBody {
    pieces: Vec<Piece>,
    /// the pack checksum so far, when it has to be computed
    hasher: Option<alt_hash::Sha1>,
    stage: Stage,
}

impl PackBody {
    pub(super) fn new(pieces: Vec<Piece>, checksum: bool) -> Self {
        Self {
            pieces,
            hasher: checksum.then(alt_hash::Sha1::new),
            stage: Stage::Announce,
        }
    }
}

/// `data` as one packet on the pack band
fn packet(data: &[u8]) -> Bytes {
    let mut packet = BytesMut::with_capacity(data.len() + 5);
    packet.put_slice(format!("{:04x}", data.len() + 5).as_bytes());
    packet.put_u8(1);
    packet.put_slice(data);
    packet.freeze()
}

impl Body for PackBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        loop {
            match this.stage {
                Stage::Announce => {
                    this.stage = Stage::Pieces { index: 0, at: 0 };
                    return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(
                        b"000dpackfile\n",
                    )))));
                }
                Stage::Pieces { index, at } => {
                    let Some(piece) = this.pieces.get(index) else {
                        this.stage = Stage::Checksum;
                        continue;
                    };
                    let bytes = piece.bytes();
                    if at == bytes.len() {
                        this.stage = Stage::Pieces {
                            index: index + 1,
                            at: 0,
                        };
                        continue;
                    }
                    let end = bytes.len().min(at + PACKET);
                    if let Some(hasher) = &mut this.hasher {
                        hasher.update(&bytes[at..end]);
                    }
                    let frame = packet(&bytes[at..end]);
                    this.stage = Stage::Pieces { index, at: end };
                    return Poll::Ready(Some(Ok(Frame::data(frame))));
                }
                Stage::Checksum => {
                    this.stage = Stage::Flush;
                    if let Some(hasher) = this.hasher.take() {
                        return Poll::Ready(Some(Ok(Frame::data(packet(&hasher.finalize())))));
                    }
                }
                Stage::Flush => {
                    this.stage = Stage::Done;
                    return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"0000")))));
                }
                Stage::Done => return Poll::Ready(None),
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self.stage, Stage::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    /// the pack bytes inside the side-band packets of a response
    fn unframed(mut response: &[u8]) -> Vec<u8> {
        assert_eq!(&response[..13], b"000dpackfile\n");
        response = &response[13..];
        let mut pack = Vec::new();
        loop {
            let len =
                usize::from_str_radix(std::str::from_utf8(&response[..4]).unwrap(), 16).unwrap();
            if len == 0 {
                assert_eq!(response.len(), 4);
                return pack;
            }
            assert!(len <= PACKET + 5);
            assert_eq!(response[4], 1);
            pack.extend_from_slice(&response[5..len]);
            response = &response[len..];
        }
    }

    #[tokio::test]
    async fn pieces_are_framed_in_order_and_followed_by_their_checksum() {
        let first: Vec<u8> = (0..200_000u32).map(|n| n as u8).collect();
        let second = vec![9u8; PACKET];
        for checksum in [false, true] {
            let pieces = vec![
                Piece::Owned(first.clone()),
                Piece::Owned(Vec::new()),
                Piece::Owned(second.clone()),
            ];
            let body = PackBody::new(pieces, checksum);
            let response = body.collect().await.unwrap().to_bytes();
            let mut expected = [first.as_slice(), second.as_slice()].concat();
            if checksum {
                let trailer = alt_hash::sha1(&expected);
                expected.extend_from_slice(&trailer);
            }
            assert_eq!(unframed(&response), expected);
        }
    }
}
