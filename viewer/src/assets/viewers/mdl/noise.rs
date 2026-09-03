//! `tPerlinNoise2D`, which the engine builds at startup rather than shipping in sqpack.

/// Texels a side. The game keeps six mips of it; the only consumer reads level 0.
pub const SIZE: usize = 256;

/// Rows and columns the second channel is read on by, which is what decorrelates it from the first.
const SHIFT: usize = 32;

/// MT19937, seeded and drawn as the generator does.
struct Twister {
    state: [u32; 624],
    at: usize,
}

impl Twister {
    fn new(seed: u32) -> Self {
        let mut state = [0u32; 624];
        state[0] = seed;
        for at in 1..state.len() {
            let last = state[at - 1];
            state[at] = 1_812_433_253u32
                .wrapping_mul(last ^ (last >> 30))
                .wrapping_add(at as u32);
        }
        let mut held = Self { state, at: 624 };
        held.twist();
        held
    }

    fn twist(&mut self) {
        for at in 0..self.state.len() {
            let mixed = (self.state[at] & 0x8000_0000) | (self.state[(at + 1) % 624] & 0x7fff_ffff);
            self.state[at] = self.state[(at + 397) % 624]
                ^ (mixed >> 1)
                ^ if mixed & 1 == 1 { 0x9908_b0df } else { 0 };
        }
        self.at = 0;
    }

    fn draw(&mut self) -> u32 {
        if self.at == self.state.len() {
            self.twist();
        }
        let mut held = self.state[self.at];
        self.at += 1;
        held ^= held >> 11;
        held ^= (held << 7) & 0x9d2c_5680;
        held ^= (held << 15) & 0xefc6_0000;
        held ^ (held >> 18)
    }
}

/// Where a coordinate reads and how much of each of the two rows or columns it takes, for the pair
/// of samples half a texel either side of it. Nought folds back onto itself rather than wrapping,
/// since the generator takes the absolute value before it truncates.
fn taps(at: usize) -> [(usize, f32); 2] {
    [at as f32 - 0.5, at as f32 + 0.5].map(|held| {
        let held = held.abs();
        ((held as usize + SIZE) % SIZE, held - held.trunc())
    })
}

/// One texel: a bilinear read of the uniform field at each of the four corners around it, summed
/// and quartered.
fn value(field: &[f32], x: usize, y: usize) -> f32 {
    let mut sum = 0.0f32;
    for (col, across) in taps(x) {
        for (row, down) in taps(y) {
            let left = (col + SIZE - 1) % SIZE;
            let above = (row + SIZE - 1) % SIZE;
            sum += down * across * field[row * SIZE + col]
                + (1.0 - down) * across * field[above * SIZE + col]
                + (1.0 - across) * down * field[row * SIZE + left]
                + (1.0 - across) * (1.0 - down) * field[above * SIZE + left];
        }
    }
    sum * 0.25
}

/// Stretches the field over the whole byte range it was drawn short of.
fn normalise(plane: &mut [u8]) {
    let low = plane.iter().copied().min().unwrap_or(0);
    let high = plane.iter().copied().max().unwrap_or(0);
    let scale = 255.0 / f32::from(high - low);
    for held in plane {
        *held = (f32::from(*held - low) * scale).min(255.0) as u8;
    }
}

/// The field, in RGBA bytes: red is what `tPerlinNoise2D` answers, green the offset copy the engine
/// pairs it with for `tPerlinNoise3D`.
pub fn perlin() -> Vec<u8> {
    let mut twister = Twister::new(42);
    let field: Vec<f32> = (0..SIZE * SIZE)
        .map(|_| (twister.draw() as f32 + 0.5) / 4_294_967_296.0)
        .collect();
    let mut plane: Vec<u8> = (0..SIZE * SIZE)
        .map(|at| {
            let held = value(&field, at % SIZE, at / SIZE);
            let held = if held >= 1.0 { 1.0 } else { held };
            let held = if held <= 0.0 { 0.0 } else { held };
            (held * 255.0) as u8
        })
        .collect();
    normalise(&mut plane);
    (0..SIZE * SIZE)
        .flat_map(|at| {
            let offset = (at / SIZE + SHIFT) % SIZE * SIZE + (at % SIZE + SHIFT) % SIZE;
            [plane[at], plane[offset], 0, 255]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published MT19937 stream for seed 42, which is what the generator seeds with.
    #[test]
    fn twister_matches_the_reference_stream() {
        let mut twister = Twister::new(42);
        let drawn: Vec<u32> = (0..6).map(|_| twister.draw()).collect();
        assert_eq!(drawn, [
            1_608_637_542,
            3_421_126_067,
            4_083_286_876,
            787_846_414,
            3_143_890_026,
            3_348_747_335,
        ]);
    }

    /// The field is stretched over the whole byte range, so its own ends land on the range's.
    #[test]
    fn the_field_spans_the_whole_byte_range() {
        let held = perlin();
        let red: Vec<u8> = held.iter().step_by(4).copied().collect();
        assert_eq!(red.len(), SIZE * SIZE);
        assert_eq!(red.iter().copied().min(), Some(0));
        assert_eq!(red.iter().copied().max(), Some(255));
    }

    /// The second channel is the first read thirty-two rows and thirty-two columns on.
    #[test]
    fn the_second_channel_is_the_first_offset() {
        let held = perlin();
        for (y, x) in [(0, 0), (7, 200), (223, 224), (255, 255), (100, 31)] {
            let at = 4 * (y * SIZE + x);
            let from = 4 * ((y + SHIFT) % SIZE * SIZE + (x + SHIFT) % SIZE);
            assert_eq!(held[at + 1], held[from], "at ({x}, {y})");
        }
    }

    /// What the field answers where the one shader reading it samples. Generated rather than read
    /// off the game, so this locks the field against a change rather than proving it.
    #[test]
    fn the_field_answers_its_own_texels() {
        let held = perlin();
        let red = |x: usize, y: usize| held[4 * (y * SIZE + x)];
        assert_eq!(red(0, 0), 71);
        assert_eq!(red(128, 128), 178);
        assert_eq!(red(255, 255), 165);
        assert_eq!(red(64, 192), 125);
    }
}
