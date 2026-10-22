//! Monotonic PTS loop controller managing seamless timestamp progression across infinite video loops.

/// Controller for monotonic presentation timestamp (PTS) calculation during infinite loops.
#[derive(Debug, Clone)]
pub struct LoopController {
    pub current_frame_idx: usize,
    pub total_frames: usize,
    pub monotonic_pts_100ns: i64,
    pub loop_count: u64,
}

impl LoopController {
    /// Creates a new loop controller with the given total frame count.
    pub fn new(total_frames: usize) -> Self {
        Self {
            current_frame_idx: 0,
            total_frames,
            monotonic_pts_100ns: 0,
            loop_count: 0,
        }
    }

    /// Resets the controller state for a new video stream.
    pub fn reset(&mut self, total_frames: usize) {
        self.current_frame_idx = 0;
        self.total_frames = total_frames;
        self.monotonic_pts_100ns = 0;
        self.loop_count = 0;
    }

    /// Advances the controller by one frame and returns `(frame_index_to_read, current_pts_100ns, wrapped_around)`.
    ///
    /// The timestamp `monotonic_pts_100ns` continuously increases on every call, preventing decoder flushes
    /// when the stream loops back to frame index 0.
    pub fn advance(&mut self, frame_duration_100ns: i64) -> (usize, i64, bool) {
        if self.total_frames == 0 {
            return (0, self.monotonic_pts_100ns, false);
        }

        let frame_to_read = self.current_frame_idx;
        let current_pts = self.monotonic_pts_100ns;
        let wrapped_around = self.loop_count > 0 && frame_to_read == 0;

        self.monotonic_pts_100ns = self
            .monotonic_pts_100ns
            .saturating_add(frame_duration_100ns);
        self.current_frame_idx += 1;

        if self.current_frame_idx >= self.total_frames {
            self.current_frame_idx = 0;
            self.loop_count += 1;
        }

        (frame_to_read, current_pts, wrapped_around)
    }

    pub fn current_frame_idx(&self) -> usize {
        self.current_frame_idx
    }

    pub fn total_frames(&self) -> usize {
        self.total_frames
    }

    pub fn monotonic_pts_100ns(&self) -> i64 {
        self.monotonic_pts_100ns
    }

    pub fn loop_count(&self) -> u64 {
        self.loop_count
    }
}

impl Default for LoopController {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monotonic_loop_progression() {
        let total_frames = 3;
        let frame_dur = 166_666; // ~60fps in 100ns units
        let mut ctrl = LoopController::new(total_frames);

        // Frame 0 (first loop)
        let (f0, pts0, wrap0) = ctrl.advance(frame_dur);
        assert_eq!(f0, 0);
        assert_eq!(pts0, 0);
        assert!(!wrap0);

        // Frame 1
        let (f1, pts1, wrap1) = ctrl.advance(frame_dur);
        assert_eq!(f1, 1);
        assert_eq!(pts1, frame_dur);
        assert!(!wrap1);

        // Frame 2 (last frame of loop 0)
        let (f2, pts2, wrap2) = ctrl.advance(frame_dur);
        assert_eq!(f2, 2);
        assert_eq!(pts2, frame_dur * 2);
        assert!(!wrap2);

        // Frame 0 (loop 1 - wrap around!)
        let (f3, pts3, wrap3) = ctrl.advance(frame_dur);
        assert_eq!(f3, 0);
        assert_eq!(pts3, frame_dur * 3);
        assert!(wrap3);
        assert_eq!(ctrl.loop_count(), 1);

        // Frame 1 (loop 1)
        let (f4, pts4, wrap4) = ctrl.advance(frame_dur);
        assert_eq!(f4, 1);
        assert_eq!(pts4, frame_dur * 4);
        assert!(!wrap4);
    }
}
