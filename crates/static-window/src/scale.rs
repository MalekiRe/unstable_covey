//! Display scale, kept exact in 120ths: fractional-scale-v1's unit, so 120 is
//! 100% and 180 is 150%.

/// A surface's scale in 120ths (`Scale(180)` is 1.5×).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scale(pub u32);

impl Scale {
    /// 120ths per 1×.
    pub const UNIT: u32 = 120;
    pub const ONE: Scale = Scale(Self::UNIT);

    /// An integer scale (`wl_output.scale`, the fallback without
    /// fractional-scale-v1).
    pub const fn integer(factor: u32) -> Self {
        Scale(factor * Self::UNIT)
    }
    pub fn factor(self) -> f32 {
        self.0 as f32 / Self::UNIT as f32
    }
    /// Whether this is a whole number, so `wl_surface.set_buffer_scale` can
    /// express it without a viewport.
    pub const fn is_integer(self) -> bool {
        self.0.is_multiple_of(Self::UNIT)
    }
    /// Pixels covering `logical` units, rounded up so a surface never
    /// resolves to fewer pixels than it covers.
    pub const fn to_pixels(self, logical: u32) -> u32 {
        (logical * self.0).div_ceil(Self::UNIT)
    }
}
impl Default for Scale {
    fn default() -> Self {
        Self::ONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_round_up_at_fractional_scales() {
        assert_eq!(Scale::ONE.to_pixels(801), 801);
        assert_eq!(Scale::integer(2).to_pixels(801), 1602);
        // 1.25 × 801 = 1001.25: the last partial pixel is still covered.
        assert_eq!(Scale(150).to_pixels(801), 1002);
        assert_eq!(Scale(150).to_pixels(800), 1000);
        assert!(Scale::integer(3).is_integer());
        assert!(!Scale(180).is_integer());
        assert_eq!(Scale(180).factor(), 1.5);
    }
}
