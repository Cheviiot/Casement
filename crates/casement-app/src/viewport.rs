//! One geometry for pointer mapping and the local cursor's visibility.

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    origin: (f64, f64),
    scale: f64,
    source: (f64, f64),
}

impl Viewport {
    pub fn new(widget: (f64, f64), source: (f64, f64)) -> Option<Self> {
        if [widget.0, widget.1, source.0, source.1]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0)
        {
            return None;
        }
        let scale = (widget.0 / source.0).min(widget.1 / source.1);
        Some(Self {
            origin: (
                (widget.0 - source.0 * scale) / 2.0,
                (widget.1 - source.1 * scale) / 2.0,
            ),
            scale,
            source,
        })
    }

    fn project(self, point: (f64, f64)) -> (f64, f64) {
        (
            (point.0 - self.origin.0) / self.scale,
            (point.1 - self.origin.1) / self.scale,
        )
    }

    pub fn contains(self, point: (f64, f64)) -> bool {
        let (x, y) = self.project(point);
        (0.0..self.source.0).contains(&x) && (0.0..self.source.1).contains(&y)
    }

    pub fn remote(self, point: (f64, f64)) -> Option<(f64, f64)> {
        if !point.0.is_finite() || !point.1.is_finite() {
            return None;
        }
        let (x, y) = self.project(point);
        Some((
            x.clamp(0.0, (self.source.0 - 1.0).max(0.0)),
            y.clamp(0.0, (self.source.1 - 1.0).max(0.0)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_cursor_and_input_agree_at_edges() {
        let viewport = Viewport::new((800.0, 600.0), (1920.0, 1080.0)).unwrap();
        assert!(!viewport.contains((400.0, 74.0)));
        assert!(viewport.contains((400.0, 75.0)));
        assert_eq!(viewport.remote((400.0, 300.0)), Some((960.0, 540.0)));
        assert_eq!(viewport.remote((-20.0, -20.0)), Some((0.0, 0.0)));
        assert_eq!(viewport.remote((900.0, 700.0)), Some((1919.0, 1079.0)));
        assert!(!viewport.contains((800.0, 300.0)));
    }

    #[test]
    fn resize_recomputes_pillarbox_without_pointer_motion() {
        let wide = Viewport::new((1200.0, 600.0), (800.0, 600.0)).unwrap();
        assert!(!wide.contains((100.0, 300.0)));
        let narrow = Viewport::new((800.0, 600.0), (800.0, 600.0)).unwrap();
        assert!(narrow.contains((100.0, 300.0)));
        assert_eq!(wide.remote((600.0, 300.0)), Some((400.0, 300.0)));
    }

    #[test]
    fn no_geometry_for_unallocated_or_invalid_sizes() {
        assert!(Viewport::new((0.0, 20.0), (800.0, 600.0)).is_none());
        assert!(Viewport::new((800.0, 600.0), (0.0, 0.0)).is_none());
        assert!(Viewport::new((f64::INFINITY, 600.0), (800.0, 600.0)).is_none());
        assert!(Viewport::new((800.0, 600.0), (800.0, 600.0))
            .unwrap()
            .remote((f64::NAN, 1.0))
            .is_none());
    }
}
