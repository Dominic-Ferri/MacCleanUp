//! Squarified treemap layout (Bruls, Huizing, van Wijk 2000). Tiles keep
//! aspect ratios near 1 so small folders stay clickable.

use eframe::egui::{pos2, Rect};

/// Returns one rectangle per weight, in input order. Weights must be sorted
/// largest first; zero weights get an empty rectangle.
pub fn layout(weights: &[u64], area: Rect) -> Vec<Rect> {
    let mut rects = vec![Rect::NOTHING; weights.len()];
    let total: f64 = weights.iter().map(|&weight| weight as f64).sum();
    if total <= 0.0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return rects;
    }
    let scale = f64::from(area.width()) * f64::from(area.height()) / total;
    let areas: Vec<f64> = weights
        .iter()
        .map(|&weight| weight as f64 * scale)
        .collect();
    let positive = areas.iter().take_while(|&&value| value > 0.0).count();

    let mut free = area;
    let mut start = 0;
    while start < positive {
        let side = f64::from(free.width().min(free.height()));
        let mut end = start + 1;
        while end < positive && worst(&areas[start..=end], side) <= worst(&areas[start..end], side)
        {
            end += 1;
        }
        free = place_row(&areas[start..end], free, &mut rects[start..end]);
        start = end;
    }
    rects
}

fn worst(row: &[f64], side: f64) -> f64 {
    let sum: f64 = row.iter().sum();
    let (min, max) = row.iter().fold((f64::MAX, 0.0_f64), |(min, max), &value| {
        (min.min(value), max.max(value))
    });
    let side2 = side * side;
    let sum2 = sum * sum;
    (side2 * max / sum2).max(sum2 / (side2 * min))
}

/// Lays `row` along the shorter side of `free` and returns the space left.
fn place_row(row: &[f64], free: Rect, out: &mut [Rect]) -> Rect {
    let sum: f64 = row.iter().sum();
    if free.width() >= free.height() {
        let width = (sum / f64::from(free.height())) as f32;
        let mut y = free.top();
        for (value, rect) in row.iter().zip(out.iter_mut()) {
            let height = (value / sum) as f32 * free.height();
            *rect = Rect::from_min_max(pos2(free.left(), y), pos2(free.left() + width, y + height));
            y += height;
        }
        Rect::from_min_max(pos2(free.left() + width, free.top()), free.max)
    } else {
        let height = (sum / f64::from(free.width())) as f32;
        let mut x = free.left();
        for (value, rect) in row.iter().zip(out.iter_mut()) {
            let width = (value / sum) as f32 * free.width();
            *rect = Rect::from_min_max(pos2(x, free.top()), pos2(x + width, free.top() + height));
            x += width;
        }
        Rect::from_min_max(pos2(free.left(), free.top() + height), free.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    #[test]
    fn tiles_cover_the_area_in_proportion_without_overlap() {
        let area = Rect::from_min_size(pos2(10.0, 20.0), vec2(600.0, 400.0));
        let weights = [600, 300, 200, 100, 50, 50, 0];
        let rects = layout(&weights, area);
        let total: u64 = weights.iter().sum();
        for (rect, weight) in rects.iter().zip(weights) {
            if weight == 0 {
                assert!(rect.area() <= 0.0 || !rect.is_positive());
                continue;
            }
            let expected = area.area() * weight as f32 / total as f32;
            assert!(
                (rect.area() - expected).abs() < 1.0,
                "{rect:?} vs {expected}"
            );
            assert!(area.expand(0.01).contains_rect(*rect));
        }
        for (index, a) in rects.iter().enumerate() {
            for b in rects.iter().skip(index + 1) {
                if a.is_positive() && b.is_positive() {
                    assert!(a.intersect(*b).area() < 0.5, "{a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn squarified_tiles_stay_close_to_square() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0));
        let rects = layout(&[6, 6, 4, 3, 2, 2, 1], area);
        for rect in rects {
            let ratio = rect.width().max(rect.height()) / rect.width().min(rect.height());
            assert!(ratio < 4.0, "{rect:?} ratio {ratio}");
        }
    }

    #[test]
    fn empty_or_zero_input_yields_no_tiles() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        assert!(layout(&[], area).is_empty());
        assert!(layout(&[0, 0], area).iter().all(|rect| !rect.is_positive()));
    }
}
