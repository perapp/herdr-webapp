//! Bounded DataChannel fragmentation. A frame is never reused across peer epochs.
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
pub const MAX_PACKET: usize = 16_384;
pub const HEADER: usize = 17;
pub const MAX_MESSAGE: usize = 16 << 20;
const MAX_PARTIAL: usize = 4;
const MAX_RESERVED: usize = 32 << 20;
const MAGIC: &[u8; 4] = b"HDR\x01";
struct Partial {
    total: usize,
    kind: u8,
    bytes: Vec<u8>,
    started: Instant,
}
pub struct FrameDecoder {
    max_message: usize,
    partial: HashMap<u32, Partial>,
    highest_started: u32,
}
impl FrameDecoder {
    pub fn new(max_message: usize) -> Self {
        Self {
            max_message: max_message.min(MAX_MESSAGE),
            partial: HashMap::new(),
            highest_started: 0,
        }
    }
    /// Check even when no further fragments arrive; expiry fails the channel closed.
    pub fn check_expired(&self) -> Result<()> {
        self.check_expired_at(Instant::now())
    }
    fn check_expired_at(&self, now: Instant) -> Result<()> {
        ensure!(
            !self
                .partial
                .values()
                .any(|p| { now.saturating_duration_since(p.started) >= Duration::from_secs(10) }),
            "Partial message timed out"
        );
        Ok(())
    }
    pub fn push(&mut self, packet: &[u8]) -> Result<Option<(u8, Vec<u8>)>> {
        self.check_expired()?;
        ensure!(
            (HEADER..=MAX_PACKET).contains(&packet.len()) && &packet[..4] == MAGIC,
            "Invalid peer frame"
        );
        let id = u32::from_le_bytes(packet[4..8].try_into()?);
        let total = u32::from_le_bytes(packet[8..12].try_into()?) as usize;
        let offset = u32::from_le_bytes(packet[12..16].try_into()?) as usize;
        let kind = packet[16];
        let data = &packet[HEADER..];
        ensure!(
            id > 0
                && total <= self.max_message
                && kind <= 1
                && offset <= total
                && data.len() <= total - offset
                && (!data.is_empty() || total == 0),
            "Peer frame exceeds bounds"
        );
        if offset == 0 {
            ensure!(
                id > self.highest_started && !self.partial.contains_key(&id),
                "Peer message replay"
            );
            ensure!(
                self.partial.len() < MAX_PARTIAL
                    && self.partial.values().map(|p| p.total).sum::<usize>() + total
                        <= MAX_RESERVED,
                "Too many partial messages"
            );
            self.highest_started = id;
            self.partial.insert(
                id,
                Partial {
                    total,
                    kind,
                    bytes: Vec::with_capacity(total),
                    started: Instant::now(),
                },
            );
        }
        let part = self
            .partial
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("Unknown fragment"))?;
        ensure!(
            part.total == total && part.kind == kind && part.bytes.len() == offset,
            "Out-of-order peer fragment"
        );
        part.bytes.extend_from_slice(data);
        if part.bytes.len() != total {
            return Ok(None);
        }
        let part = self.partial.remove(&id).unwrap();
        if kind == 0 {
            std::str::from_utf8(&part.bytes).map_err(|_| anyhow::anyhow!("Invalid peer text"))?;
        }
        Ok(Some((kind, part.bytes)))
    }
}
pub fn encode(kind: u8, message_id: u32, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    ensure!(
        kind <= 1 && message_id != 0 && bytes.len() <= MAX_MESSAGE,
        "Invalid outgoing peer message"
    );
    if kind == 0 {
        std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("Invalid outgoing peer text"))?;
    }
    let mut frames = Vec::new();
    let mut offset = 0;
    loop {
        let end = (offset + MAX_PACKET - HEADER).min(bytes.len());
        let mut packet = Vec::with_capacity(HEADER + end - offset);
        packet.extend_from_slice(MAGIC);
        packet.extend(message_id.to_le_bytes());
        packet.extend((bytes.len() as u32).to_le_bytes());
        packet.extend((offset as u32).to_le_bytes());
        packet.push(kind);
        packet.extend_from_slice(&bytes[offset..end]);
        frames.push(packet);
        if end == bytes.len() {
            break;
        }
        offset = end;
    }
    Ok(frames)
}
#[cfg(test)]
fn next_id(last: &mut u32) -> Result<u32> {
    if *last == u32::MAX {
        anyhow::bail!("Peer channel message IDs exhausted");
    }
    *last += 1;
    Ok(*last)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_big_terminal_frame_and_empty() {
        let bytes = vec![42; 262_144];
        let frames = encode(1, 1, &bytes).unwrap();
        assert!(frames.len() > 16 && frames.iter().all(|p| p.len() <= MAX_PACKET));
        let mut decoder = FrameDecoder::new(1 << 20);
        let mut got = None;
        for frame in frames {
            got = decoder.push(&frame).unwrap();
        }
        assert_eq!(got.unwrap(), (1, bytes));
        assert_eq!(
            decoder.push(&encode(0, 2, b"").unwrap()[0]).unwrap(),
            Some((0, vec![]))
        );
    }
    #[test]
    fn rejects_replay_gap_bad_kind_and_oversize() {
        let frame = encode(0, 1, b"hello").unwrap().remove(0);
        let mut decoder = FrameDecoder::new(64);
        decoder.push(&frame).unwrap();
        assert!(decoder.push(&frame).is_err());
        let mut bad = frame.clone();
        bad[12] = 1;
        assert!(FrameDecoder::new(64).push(&bad).is_err());
        bad = frame.clone();
        bad[16] = 9;
        assert!(FrameDecoder::new(64).push(&bad).is_err());
        assert!(FrameDecoder::new(2).push(&frame).is_err());
        assert!(FrameDecoder::new(64).push(b"short").is_err());
    }
    #[test]
    fn bounds_partial_messages_and_checks_offsets() {
        let mut decoder = FrameDecoder::new(MAX_MESSAGE);
        let frames = encode(1, 1, &vec![1; 20000]).unwrap();
        decoder.push(&frames[0]).unwrap();
        let mut wrong = frames[1].clone();
        wrong[12..16].copy_from_slice(&1u32.to_le_bytes());
        assert!(decoder.push(&wrong).is_err());
        for id in 2..=4 {
            decoder
                .push(&encode(1, id, &vec![1; 20000]).unwrap()[0])
                .unwrap();
        }
        assert!(
            decoder
                .push(&encode(1, 5, &vec![1; 20000]).unwrap()[0])
                .is_err()
        );
    }
    #[test]
    fn idle_partial_expires_at_deadline_but_completed_messages_do_not() {
        let mut decoder = FrameDecoder::new(30000);
        let frames = encode(1, 1, &vec![0; 20000]).unwrap();
        decoder.push(&frames[0]).unwrap();
        let started = decoder.partial[&1].started;
        assert!(
            decoder
                .check_expired_at(started + Duration::from_millis(9999))
                .is_ok()
        );
        assert!(
            decoder
                .check_expired_at(started + Duration::from_secs(10))
                .is_err()
        );
        decoder.push(&frames[1]).unwrap();
        assert!(
            decoder
                .check_expired_at(started + Duration::from_secs(60))
                .is_ok()
        );
    }
    #[test]
    fn expired_partial_and_counter_exhaustion_fail_closed() {
        let mut decoder = FrameDecoder::new(30000);
        let frames = encode(1, 1, &vec![0; 20000]).unwrap();
        decoder.push(&frames[0]).unwrap();
        decoder.partial.get_mut(&1).unwrap().started = Instant::now() - Duration::from_secs(11);
        assert!(decoder.push(&frames[1]).is_err());
        let mut id = u32::MAX;
        assert!(next_id(&mut id).is_err());
    }
}
