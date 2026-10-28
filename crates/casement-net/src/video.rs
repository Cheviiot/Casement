//! Keep the decoder's compressed-input queue short without breaking a H.264 chain.

pub const DECODE_QUEUE: u64 = 3;

#[derive(Default)]
pub struct VideoBudget {
    pub recovering: bool,
}

impl VideoBudget {
    pub fn accept(&mut self, keyframe: bool, queued: u64) -> bool {
        if queued >= DECODE_QUEUE {
            self.recovering = true;
            return false;
        }
        if self.recovering && !keyframe {
            return false;
        }
        if keyframe {
            self.recovering = false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_pressure_skips_to_a_keyframe_then_preserves_deltas() {
        let mut budget = VideoBudget::default();
        assert!(budget.accept(true, 0));
        assert!(budget.accept(false, 1));
        assert!(!budget.accept(false, DECODE_QUEUE));
        assert!(budget.recovering);
        assert!(!budget.accept(false, 0));
        assert!(!budget.accept(true, DECODE_QUEUE));
        assert!(budget.recovering);
        assert!(budget.accept(true, 0));
        assert!(!budget.recovering);
        assert!(budget.accept(false, 1));
    }
}
