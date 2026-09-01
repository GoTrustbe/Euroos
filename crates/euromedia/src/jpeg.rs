//! Baseline JPEG (JFIF) decoder: sequential DCT, Huffman entropy coding,
//! 8-bit precision, grayscale or YCbCr with 4:4:4 / 4:2:2 / 4:2:0 subsampling,
//! restart markers. Progressive (SOF2), arithmetic coding and 12-bit are
//! honestly rejected as Unsupported. From scratch, no_std + alloc.

use crate::Image;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, PartialEq, Eq)]
pub enum JpegError {
    NotJpeg,
    Unsupported,
    Truncated,
    BadData,
}

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

// ── Huffman table: canonical codes from the DHT (bits, values) spec ─────────
struct Huffman {
    /// For each code length 1..=16: (first code, first value index).
    min_code: [i32; 17],
    max_code: [i32; 17],
    val_ptr: [usize; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn build(bits: &[u8; 16], values: Vec<u8>) -> Huffman {
        let mut min_code = [0i32; 17];
        let mut max_code = [-1i32; 17];
        let mut val_ptr = [0usize; 17];
        let mut code = 0i32;
        let mut k = 0usize;
        for l in 1..=16usize {
            if bits[l - 1] == 0 {
                max_code[l] = -1;
            } else {
                val_ptr[l] = k;
                min_code[l] = code;
                code += bits[l - 1] as i32;
                k += bits[l - 1] as usize;
                max_code[l] = code - 1;
            }
            code <<= 1;
        }
        Huffman { min_code, max_code, val_ptr, values }
    }
}

// ── Bit reader over the entropy-coded segment (0xFF00 unstuffing) ───────────
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit_buf: u32,
    bit_cnt: u32,
    /// Set when a marker (RSTn or EOI) interrupted the stream.
    marker: Option<u8>,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, bit_buf: 0, bit_cnt: 0, marker: None }
    }

    fn fill(&mut self) {
        while self.bit_cnt <= 24 {
            if self.marker.is_some() {
                // After a marker: feed zero bits (the scan is over / resyncs).
                self.bit_buf |= 0 << (24 - self.bit_cnt);
                self.bit_cnt += 8;
                continue;
            }
            if self.pos >= self.data.len() {
                self.bit_cnt += 8; // pad with zeros at EOF
                continue;
            }
            let b = self.data[self.pos];
            self.pos += 1;
            if b == 0xFF {
                let n = self.data.get(self.pos).copied().unwrap_or(0);
                if n == 0x00 {
                    self.pos += 1; // stuffed 0xFF data byte
                } else {
                    // A real marker: remember it, do not consume data past it.
                    self.marker = Some(n);
                    self.pos += 1;
                    self.bit_cnt += 8;
                    continue;
                }
            }
            self.bit_buf |= (b as u32) << (24 - self.bit_cnt);
            self.bit_cnt += 8;
        }
    }

    fn get_bit(&mut self) -> u32 {
        if self.bit_cnt == 0 {
            self.fill();
        }
        let bit = self.bit_buf >> 31;
        self.bit_buf <<= 1;
        self.bit_cnt -= 1;
        bit
    }

    fn get_bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.get_bit();
        }
        v
    }

    /// Reset at a restart marker: drop buffered bits and CONSUME the RSTn
    /// marker itself. The eager prefetch may or may not have reached it yet —
    /// when it has not, scan forward to the marker (everything before it is
    /// 1-bit padding by spec).
    fn restart(&mut self) {
        self.bit_buf = 0;
        self.bit_cnt = 0;
        if self.marker.take().is_some() {
            return; // prefetch already consumed the marker
        }
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == 0xFF {
                let n = self.data[self.pos + 1];
                if (0xD0..=0xD7).contains(&n) {
                    self.pos += 2; // consume RSTn
                    return;
                }
                if n == 0x00 {
                    self.pos += 2; // stuffed data byte (padding area)
                    continue;
                }
            }
            self.pos += 1;
        }
    }

    fn decode_huff(&mut self, h: &Huffman) -> Result<u8, JpegError> {
        let mut code = 0i32;
        for l in 1..=16usize {
            code = (code << 1) | self.get_bit() as i32;
            if h.max_code[l] >= 0 && code <= h.max_code[l] && code >= h.min_code[l] {
                let idx = h.val_ptr[l] + (code - h.min_code[l]) as usize;
                return h.values.get(idx).copied().ok_or(JpegError::BadData);
            }
        }
        Err(JpegError::BadData)
    }
}

/// JPEG "extend": an s-bit magnitude value into its signed coefficient.
fn extend(v: u32, s: u32) -> i32 {
    if s == 0 {
        return 0;
    }
    if v < (1 << (s - 1)) {
        v as i32 - (1 << s) + 1
    } else {
        v as i32
    }
}

// ── 8×8 inverse DCT (separable, f32 with fixed constants) ───────────────────
fn idct8x8(block: &[i32; 64], out: &mut [i32; 64]) {
    // cos((2x+1) u pi / 16) table, u=0..7 — precomputed constants.
    // Some entries equal 1/sqrt(2) by arithmetic, but they are cosine values at
    // specific angles, not that constant: writing FRAC_1_SQRT_2 in a few cells of
    // an otherwise numeric table would hide the pattern the table exists to show.
    #[allow(clippy::approx_constant)]
    const C: [[f32; 8]; 8] = [
        [1.0, 0.980785, 0.923880, 0.831470, 0.707107, 0.555570, 0.382683, 0.195090],
        [1.0, 0.831470, 0.382683, -0.195090, -0.707107, -0.980785, -0.923880, -0.555570],
        [1.0, 0.555570, -0.382683, -0.980785, -0.707107, 0.195090, 0.923880, 0.831470],
        [1.0, 0.195090, -0.923880, -0.555570, 0.707107, 0.831470, -0.382683, -0.980785],
        [1.0, -0.195090, -0.923880, 0.555570, 0.707107, -0.831470, -0.382683, 0.980785],
        [1.0, -0.555570, -0.382683, 0.980785, -0.707107, -0.195090, 0.923880, -0.831470],
        [1.0, -0.831470, 0.382683, 0.195090, -0.707107, 0.980785, -0.923880, 0.555570],
        [1.0, -0.980785, 0.923880, -0.831470, 0.707107, -0.555570, 0.382683, -0.195090],
    ];
    let mut tmp = [0f32; 64];
    // Rows of the coefficient block are u (vertical freq), columns v (horizontal).
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0f32;
            for v in 0..8 {
                let cv = if v == 0 { 0.353553 } else { 0.5 };
                s += cv * C[x][v] * block[y * 8 + v] as f32;
            }
            tmp[y * 8 + x] = s;
        }
    }
    for x in 0..8 {
        for y in 0..8 {
            let mut s = 0f32;
            for u in 0..8 {
                let cu = if u == 0 { 0.353553 } else { 0.5 };
                s += cu * C[y][u] * tmp[u * 8 + x];
            }
            let v = s + 128.0;
            out[y * 8 + x] = if v < 0.0 {
                0
            } else if v > 255.0 {
                255
            } else {
                v as i32
            };
        }
    }
}

struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
    dc_pred: i32,
    /// Decoded plane at (width_blocks*8 × height_blocks*8) component resolution.
    plane: Vec<u8>,
    plane_w: usize,
    plane_h: usize,
}

/// Decode a baseline JPEG into RGBA.
pub fn decode_jpeg(data: &[u8]) -> Result<Image, JpegError> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(JpegError::NotJpeg);
    }
    let mut pos = 2usize;
    let mut qtables: [[u16; 64]; 4] = [[0; 64]; 4];
    let mut dc_tables: Vec<Option<Huffman>> = (0..4).map(|_| None).collect();
    let mut ac_tables: Vec<Option<Huffman>> = (0..4).map(|_| None).collect();
    let mut comps: Vec<Component> = Vec::new();
    let (mut width, mut height) = (0usize, 0usize);
    let mut restart_interval = 0usize;

    loop {
        if pos + 4 > data.len() {
            return Err(JpegError::Truncated);
        }
        if data[pos] != 0xFF {
            return Err(JpegError::BadData);
        }
        let marker = data[pos + 1];
        pos += 2;
        match marker {
            0xD8 => continue,          // SOI (again)
            0x01 | 0xD0..=0xD7 => continue, // TEM / RSTn outside a scan
            0xC0 => {
                // SOF0: baseline.
                let len = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
                let seg = &data[pos + 2..pos + len];
                if seg[0] != 8 {
                    return Err(JpegError::Unsupported); // 12-bit
                }
                height = ((seg[1] as usize) << 8) | seg[2] as usize;
                width = ((seg[3] as usize) << 8) | seg[4] as usize;
                let nc = seg[5] as usize;
                if nc != 1 && nc != 3 {
                    return Err(JpegError::Unsupported);
                }
                for c in 0..nc {
                    let b = &seg[6 + c * 3..9 + c * 3];
                    comps.push(Component {
                        id: b[0],
                        h: (b[1] >> 4) as usize,
                        v: (b[1] & 0xF) as usize,
                        tq: b[2] as usize,
                        td: 0,
                        ta: 0,
                        dc_pred: 0,
                        plane: Vec::new(),
                        plane_w: 0,
                        plane_h: 0,
                    });
                }
                pos += len;
            }
            0xC1 | 0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(JpegError::Unsupported); // progressive & friends
            }
            0xC4 => {
                // DHT — possibly several tables in one segment.
                let len = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
                let mut p = pos + 2;
                let end = pos + len;
                while p < end {
                    let tc = data[p] >> 4;
                    let th = (data[p] & 0xF) as usize;
                    let mut bits = [0u8; 16];
                    bits.copy_from_slice(&data[p + 1..p + 17]);
                    let total: usize = bits.iter().map(|&b| b as usize).sum();
                    let values = data[p + 17..p + 17 + total].to_vec();
                    let table = Huffman::build(&bits, values);
                    if tc == 0 {
                        dc_tables[th] = Some(table);
                    } else {
                        ac_tables[th] = Some(table);
                    }
                    p += 17 + total;
                }
                pos += len;
            }
            0xDB => {
                // DQT — one or more tables.
                let len = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
                let mut p = pos + 2;
                let end = pos + len;
                while p < end {
                    let pq = data[p] >> 4;
                    let tq = (data[p] & 0xF) as usize;
                    p += 1;
                    // Indexed on purpose: `p` advances by one or two bytes per
                    // entry depending on the precision, so this walks the table
                    // slot by slot rather than iterating a slice.
                    #[allow(clippy::needless_range_loop)]
                    for i in 0..64 {
                        let v = if pq == 0 {
                            let v = data[p] as u16;
                            p += 1;
                            v
                        } else {
                            let v = ((data[p] as u16) << 8) | data[p + 1] as u16;
                            p += 2;
                            v
                        };
                        qtables[tq][i] = v;
                    }
                }
                pos += len;
            }
            0xDD => {
                let len = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
                restart_interval = ((data[pos + 2] as usize) << 8) | data[pos + 3] as usize;
                pos += len;
            }
            0xDA => {
                // SOS: per-scan table selectors, then the entropy-coded data.
                let len = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
                let seg = &data[pos + 2..pos + len];
                let ns = seg[0] as usize;
                for i in 0..ns {
                    let cid = seg[1 + i * 2];
                    let tt = seg[2 + i * 2];
                    if let Some(c) = comps.iter_mut().find(|c| c.id == cid) {
                        c.td = (tt >> 4) as usize;
                        c.ta = (tt & 0xF) as usize;
                    }
                }
                pos += len;
                // Decode the scan.
                return decode_scan(
                    &data[pos..],
                    width,
                    height,
                    &mut comps,
                    &qtables,
                    &dc_tables,
                    &ac_tables,
                    restart_interval,
                );
            }
            0xD9 => return Err(JpegError::Truncated), // EOI before SOS
            _ => {
                // APPn/COM/other: skip by length.
                if pos + 2 > data.len() {
                    return Err(JpegError::Truncated);
                }
                let len = ((data[pos] as usize) << 8) | data[pos + 1] as usize;
                pos += len;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    scan: &[u8],
    width: usize,
    height: usize,
    comps: &mut [Component],
    qtables: &[[u16; 64]; 4],
    dc_tables: &[Option<Huffman>],
    ac_tables: &[Option<Huffman>],
    restart_interval: usize,
) -> Result<Image, JpegError> {
    if width == 0 || height == 0 || comps.is_empty() {
        return Err(JpegError::BadData);
    }
    let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
    if hmax == 0 || vmax == 0 || hmax > 2 || vmax > 2 {
        return Err(JpegError::Unsupported);
    }
    let mcus_x = width.div_ceil(8 * hmax);
    let mcus_y = height.div_ceil(8 * vmax);
    for c in comps.iter_mut() {
        c.plane_w = mcus_x * c.h * 8;
        c.plane_h = mcus_y * c.v * 8;
        c.plane = vec![0u8; c.plane_w * c.plane_h];
        c.dc_pred = 0;
    }

    let mut br = BitReader::new(scan);
    let mut block;
    let mut pixels = [0i32; 64];
    let mut mcu_count = 0usize;

    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart_interval > 0 && mcu_count > 0 && mcu_count.is_multiple_of(restart_interval) {
                // Byte-align to the RSTn marker + reset the DC predictors.
                br.restart();
                for c in comps.iter_mut() {
                    c.dc_pred = 0;
                }
            }
            #[allow(clippy::needless_range_loop)]
            for ci in 0..comps.len() {
                let (h, v, tq, td, ta) =
                    (comps[ci].h, comps[ci].v, comps[ci].tq, comps[ci].td, comps[ci].ta);
                let dc = dc_tables[td].as_ref().ok_or(JpegError::BadData)?;
                let ac = ac_tables[ta].as_ref().ok_or(JpegError::BadData)?;
                for by in 0..v {
                    for bx in 0..h {
                        // DC coefficient.
                        block = [0i32; 64];
                        let s = br.decode_huff(dc)? as u32;
                        let diff = extend(br.get_bits(s), s);
                        comps[ci].dc_pred += diff;
                        block[0] = comps[ci].dc_pred * qtables[tq][0] as i32;
                        // AC coefficients.
                        let mut k = 1usize;
                        while k < 64 {
                            let rs = br.decode_huff(ac)?;
                            let r = (rs >> 4) as usize;
                            let s = (rs & 0xF) as u32;
                            if s == 0 {
                                if r == 15 {
                                    k += 16; // ZRL
                                    continue;
                                }
                                break; // EOB
                            }
                            k += r;
                            if k > 63 {
                                return Err(JpegError::BadData);
                            }
                            let coeff = extend(br.get_bits(s), s);
                            block[ZIGZAG[k]] = coeff * qtables[tq][k] as i32;
                            k += 1;
                        }
                        idct8x8(&block, &mut pixels);
                        // Place the block in the component plane.
                        let px0 = (mx * h + bx) * 8;
                        let py0 = (my * v + by) * 8;
                        let pw = comps[ci].plane_w;
                        for y in 0..8 {
                            for x in 0..8 {
                                comps[ci].plane[(py0 + y) * pw + px0 + x] = pixels[y * 8 + x] as u8;
                            }
                        }
                    }
                }
            }
            mcu_count += 1;
        }
    }

    // Colour conversion + upsampling (nearest) into the RGBA image.
    let mut img = Image::new(width as u32, height as u32, [0, 0, 0, 255]);
    if comps.len() == 1 {
        let c = &comps[0];
        for y in 0..height {
            for x in 0..width {
                let g = c.plane[y * c.plane_w + x];
                img.set(x as u32, y as u32, [g, g, g, 255]);
            }
        }
    } else {
        let (yc, cb, cr) = (&comps[0], &comps[1], &comps[2]);
        // Triangle-filtered chroma upsampling (what libjpeg calls "fancy"):
        // sample the subsampled plane at the pixel centre with bilinear
        // weights, so chroma edges do not turn blocky.
        let sample = |c: &Component, x: usize, y: usize| -> i32 {
            // Pixel-centre mapping: src = (x + 0.5) * (h/hmax) - 0.5, in 1/128
            // fixed point. For h == hmax this is exactly x (identity).
            let fx = ((128 * c.h * x + 64 * c.h) as i32 - 64 * hmax as i32) / hmax as i32;
            let fy = ((128 * c.v * y + 64 * c.v) as i32 - 64 * vmax as i32) / vmax as i32;
            let (fx, fy) = (fx.max(0), fy.max(0));
            let (x0, y0) = ((fx >> 7) as usize, (fy >> 7) as usize);
            let (dx, dy) = (fx & 127, fy & 127);
            let x1 = (x0 + 1).min(c.plane_w - 1);
            let y1 = (y0 + 1).min(c.plane_h - 1);
            let p00 = c.plane[y0 * c.plane_w + x0] as i32;
            let p10 = c.plane[y0 * c.plane_w + x1] as i32;
            let p01 = c.plane[y1 * c.plane_w + x0] as i32;
            let p11 = c.plane[y1 * c.plane_w + x1] as i32;
            let top = p00 * (128 - dx) + p10 * dx;
            let bot = p01 * (128 - dx) + p11 * dx;
            (top * (128 - dy) + bot * dy) >> 14
        };
        for y in 0..height {
            for x in 0..width {
                let lum = yc.plane[(y * yc.v / vmax) * yc.plane_w + (x * yc.h / hmax)] as i32;
                let b = sample(cb, x, y) - 128;
                let r = sample(cr, x, y) - 128;
                // BT.601 fixed point.
                let rr = lum + ((91881 * r) >> 16);
                let gg = lum - ((22554 * b + 46802 * r) >> 16);
                let bb = lum + ((116130 * b) >> 16);
                let cl = |v: i32| v.clamp(0, 255) as u8;
                img.set(x as u32, y as u32, [cl(rr), cl(gg), cl(bb), 255]);
            }
        }
    }
    Ok(img)
}
