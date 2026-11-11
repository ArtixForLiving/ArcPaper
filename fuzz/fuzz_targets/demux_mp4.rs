#![no_main]

use arc_paper::arc_player::{Demuxer, LoopController};
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let owned = data.to_vec();
    let len = owned.len() as u64;
    let mut demuxer = Demuxer::new();
    if demuxer.open_reader(Cursor::new(owned), len).is_ok() {
        let mut loop_ctrl = LoopController::new(demuxer.frame_count());
        let steps = demuxer.frame_count().min(16);
        for _ in 0..steps {
            if demuxer.read_next_annexb_packet(&mut loop_ctrl).is_err() {
                break;
            }
        }
    }
});
