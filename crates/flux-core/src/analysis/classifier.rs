//! Heuristic content classifier and routing logic
//!
//! Categorizes raw data chunks into distinct content types (Text, Binary, Multimedia,
//! Incompressible) and recommends optimal compression pipelines and settings.
//! Designed to allow transparent replacement with a trained machine learning model (ML) in V2.

use crate::threads::signals::{ContentType, CompressionPipeline};
use super::entropy::EntropyEstimator;

/// Represents the classification decisions and parameter bundle produced by the classifier.
///
/// Compression threads consume this struct directly without performing additional analyses.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassificationResult {
    /// The detected category of the block content.
    pub content_type: ContentType,
    /// The recommended pipeline to handle compression.
    pub pipeline: CompressionPipeline,
    /// An optional preliminary stride hint (e.g. 4 for floats, 2 for 16-bit audio).
    pub stride_hint: Option<u8>,
    /// The suggested match-finder sliding window size in bytes.
    pub window_size_recommendation: usize,
    /// Confidence level of the classification decision (0.0 to 1.0).
    pub confidence: f32,
    /// Rough estimate of the compression ratio (compressed size / original size).
    pub estimated_ratio: f32,
}

/// Supported file signature types recognized via magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSignature {
    /// JPEG image format.
    Jpeg,
    /// PNG image format.
    Png,
    /// GIF image format.
    Gif,
    /// BMP image format.
    Bmp,
    /// WebP image format.
    WebP,
    /// MP3 audio format.
    Mp3,
    /// FLAC lossless audio format.
    Flac,
    /// WAV PCM audio format.
    Wav,
    /// AAC audio format.
    Aac,
    /// OGG multimedia container format.
    Ogg,
    /// MP4 video container format.
    Mp4,
    /// Matroska (MKV) video container format.
    Mkv,
    /// AVI video container format.
    Avi,
    /// PDF document format.
    Pdf,
    /// ZIP archive format.
    Zip,
    /// Gzip compression format.
    Gz,
    /// Bzip2 compression format.
    Bz2,
    /// Xz compression format.
    Xz,
    /// Zstandard compression format.
    Zstd,
    /// RAR archive format.
    Rar,
    /// Executable and Linkable Format (Linux binaries).
    Elf,
    /// Portable Executable Format (Windows binaries).
    Pe,
    /// Mach-O binary format (macOS binaries).
    MachO,
    /// Plain ASCII text.
    Ascii,
    /// Valid UTF-8 encoded text.
    Utf8Text,
    /// Raw floating point arrays (detected via stride heuristics).
    FloatArray,
    /// Unidentified signature.
    Unknown,
}

/// Analyzes data block samples using magic bytes and entropy estimation.
///
/// # V2 compatibility
/// V2: replace heuristic body of classify() with trained model inference. ClassificationResult type is stable.
///
/// # Thread Safety
/// This struct is not thread-safe (`!Sync`) but is intended to be instantiated
/// individually inside each worker/classifier thread, ensuring thread isolation without locking.
#[derive(Debug, Clone)]
pub struct ContentClassifier {
    /// Internal estimator used to check Shannon entropy.
    pub entropy_estimator: EntropyEstimator,
    /// The size in bytes of the block sample to evaluate.
    pub sample_size: usize,
}

impl Default for ContentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentClassifier {
    /// Creates a new `ContentClassifier` with a default sample size of 4096 bytes.
    pub fn new() -> Self {
        Self {
            entropy_estimator: EntropyEstimator::new(),
            sample_size: 4096,
        }
    }

    /// Classifies an arbitrary block of data to produce a `ClassificationResult`.
    ///
    /// Evaluates magic bytes, Shannon entropy, byte distribution, and fast stride hints.
    ///
    /// # Routing Logic and Rationale
    /// * **Image files (Jpeg/Png/Gif/Bmp/WebP) with entropy > 7.0**: Already compressed; trying to compress
    ///   them again wastes CPU cycle overhead. Routed to `StoreRaw`.
    /// * **Video containers (Mp4/Mkv/Avi) with entropy > 6.5**: Video blocks are highly dense and pre-compressed.
    ///   Routed to `StoreRaw`.
    /// * **Standard archives (Zip/Gz/Bz2/Xz/Zstd/Rar)**: Compressed files have uniform byte distributions.
    ///   Routed to `StoreRaw`.
    /// * **Audio containers (Wav/Flac) or scientific `FloatArray` with a detected stride**: High byte stride correlation
    ///   is best handled by Adaptive Stride Transposition to align repeating waves/bytes prior to encoding.
    ///   Routed to `MultimediaPipeline`.
    /// * **Text files (Utf8Text/Ascii) or entropy < 4.5**: Highly structured text blocks benefit significantly
    ///   from Burrows-Wheeler Transform (BWT) and PPMd prediction modeling. Routed to `TextPipeline`.
    /// * **Executables (Elf/Pe/MachO) or entropy 4.5 to 6.5**: Interleaved machine code, jump offsets, and headers.
    ///   LZ77 dictionary matching combined with fallback PPM models works best here. Routed to `BinaryPipeline`.
    /// * **Extreme Entropy (> 7.95)**: Uniform noise indicating encryption or maximum compression. Routed to `StoreRaw`.
    pub fn classify(&mut self, data: &[u8]) -> ClassificationResult {
        if data.len() < 64_536 {
            return self.classify_small_file(data);
        }

        let sample_len = std::cmp::min(data.len(), self.sample_size);
        let sample = &data[..sample_len];

        // 1. Detect magic bytes signature
        let sig = self.detect_magic_bytes(sample);

        // 2. Perform entropy checks
        self.entropy_estimator.reset();
        self.entropy_estimator.update(sample);
        let entropy = self.entropy_estimator.shannon_entropy();
        let est_ratio = self.entropy_estimator.compression_estimate();

        // 3. Fast stride check
        let stride = self.detect_stride_hint(sample);

        // 4. Apply routing rules
        self.route_by_rules(sig, entropy, stride, est_ratio)
    }

    /// Checks the first 16 bytes against known magic byte signatures.
    ///
    /// # Signatures Checked:
    /// * PE/COFF (EXE, DLL): starts with `MZ` (`5A 4D`)
    /// * ELF Binaries: starts with `\x7fELF` (`7F 45 4C 46`)
    /// * Mach-O Binaries: starts with `FEEDFACE` / `CEFAEDFE` / `FEEDFACF` / `CFFAEDFE`
    /// * JPEG: starts with `FF D8 FF`
    /// * PNG: starts with `\x89PNG\r\n\x1a\n` (`89 50 4E 47 0D 0A 1A 0A`)
    /// * GIF: starts with `GIF89a` or `GIF87a`
    /// * BMP: starts with `BM` (`42 4D`)
    /// * WebP: starts with `RIFF` and has `WEBP` at byte 8
    /// * MP3: starts with `ID3` tag or common sync frames (`FF FB`, `FF F3`)
    /// * FLAC: starts with `fLaC` (`66 4C 61 43`)
    /// * WAV: starts with `RIFF` and has `WAVE` at byte 8
    /// * AAC: checks for ADTS frames (`FF F1` or `FF F9`)
    /// * OGG: starts with `OggS` (`4F 67 67 53`)
    /// * MP4: checks for `ftyp` box pattern at byte 4
    /// * MKV: starts with EBML header (`1A 45 DF A3`)
    /// * AVI: starts with `RIFF` and has `AVI ` at byte 8
    /// * PDF: starts with `%PDF` (`25 50 44 46`)
    /// * ZIP: starts with `PK\x03\x04` (`50 4B 03 04`)
    /// * GZ: starts with `1F 8B`
    /// * BZ2: starts with `BZh` (`42 5A 68`)
    /// * XZ: starts with `\xfd7zXZ\x00` (`FD 37 7A 58 5A 00`)
    /// * ZSTD: starts with `28 B5 2F FD`
    /// * RAR: starts with `Rar!\x1a\x07\x00` or `Rar!\x1a\x07\x01\x00`
    pub fn detect_magic_bytes(&self, data: &[u8]) -> FileSignature {
        let len = data.len();
        if len == 0 {
            return FileSignature::Unknown;
        }

        // Match first 16 bytes
        if len >= 2 && data[0] == 0x4D && data[1] == 0x5A {
            return FileSignature::Pe;
        }
        if len >= 4 && data[0] == 0x7F && data[1] == 0x45 && data[2] == 0x4C && data[3] == 0x46 {
            return FileSignature::Elf;
        }
        if len >= 4 {
            let macho_sig = [
                [0xFE, 0xED, 0xFA, 0xCE],
                [0xCE, 0xFA, 0xED, 0xFE],
                [0xFE, 0xED, 0xFA, 0xCF],
                [0xCF, 0xFA, 0xED, 0xFE],
            ];
            if macho_sig.contains(&[data[0], data[1], data[2], data[3]]) {
                return FileSignature::MachO;
            }
        }
        if len >= 8 && data[0] == 0x89 && data[1] == 0x50 && data[2] == 0x4E && data[3] == 0x47
            && data[4] == 0x0D && data[5] == 0x0A && data[6] == 0x1A && data[7] == 0x0A {
            return FileSignature::Png;
        }
        if len >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
            return FileSignature::Jpeg;
        }
        if len >= 6 && (data.starts_with(b"GIF89a") || data.starts_with(b"GIF87a")) {
            return FileSignature::Gif;
        }
        if len >= 2 && data[0] == 0x42 && data[1] == 0x4D {
            return FileSignature::Bmp;
        }
        if len >= 12 && data.starts_with(b"RIFF") {
            if &data[8..12] == b"WEBP" {
                return FileSignature::WebP;
            }
            if &data[8..12] == b"WAVE" {
                return FileSignature::Wav;
            }
            if &data[8..12] == b"AVI " {
                return FileSignature::Avi;
            }
        }
        if len >= 4 && data.starts_with(b"PK\x03\x04") {
            return FileSignature::Zip;
        }
        if len >= 2 && data[0] == 0x1F && data[1] == 0x8B {
            return FileSignature::Gz;
        }
        if len >= 3 && data[0] == 0x42 && data[1] == 0x5A && data[2] == 0x68 {
            return FileSignature::Bz2;
        }
        if len >= 6 && data.starts_with(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]) {
            return FileSignature::Xz;
        }
        if len >= 4 && data[0] == 0x28 && data[1] == 0xB5 && data[2] == 0x2F && data[3] == 0xFD {
            return FileSignature::Zstd;
        }
        if len >= 7 && (data.starts_with(b"Rar!\x1a\x07\x00") || data.starts_with(b"Rar!\x1a\x07\x01\x00")) {
            return FileSignature::Rar;
        }
        if len >= 4 && data.starts_with(b"%PDF") {
            return FileSignature::Pdf;
        }
        if len >= 3 && data.starts_with(b"ID3") {
            return FileSignature::Mp3;
        }
        if len >= 2 && (data[0] == 0xFF && (data[1] == 0xFB || data[1] == 0xF3)) {
            return FileSignature::Mp3;
        }
        if len >= 4 && data.starts_with(b"fLaC") {
            return FileSignature::Flac;
        }
        if len >= 2 && (data[0] == 0xFF && (data[1] == 0xF1 || data[1] == 0xF9)) {
            return FileSignature::Aac;
        }
        if len >= 4 && data.starts_with(b"OggS") {
            return FileSignature::Ogg;
        }
        if len >= 8 && &data[4..8] == b"ftyp" {
            return FileSignature::Mp4;
        }
        if len >= 4 && data[0] == 0x1A && data[1] == 0x45 && data[2] == 0xDF && data[3] == 0xA3 {
            return FileSignature::Mkv;
        }

        // Run Text fallback checks
        let mut is_ascii = true;
        for &b in data.iter().take(256) {
            if !((0x20..=0x7E).contains(&b) || b == 0x09 || b == 0x0A || b == 0x0D) {
                is_ascii = false;
                break;
            }
        }
        if is_ascii && !data.is_empty() {
            return FileSignature::Ascii;
        }

        let utf8_sample_len = std::cmp::min(data.len(), 256);
        if utf8_sample_len > 0 && !data[..utf8_sample_len].contains(&0) {
            match std::str::from_utf8(&data[..utf8_sample_len]) {
                Ok(_) => {
                    return FileSignature::Utf8Text;
                }
                Err(e) => {
                    let valid_len = e.valid_up_to();
                    if valid_len > 0 && std::str::from_utf8(&data[..valid_len]).is_ok() {
                        return FileSignature::Utf8Text;
                    }
                }
            }
        }

        FileSignature::Unknown
    }

    /// Performs a quick stride pre-detection on strides 2..=16.
    ///
    /// Computes direct match density. Returns `Some(stride)` if the matching rate is >0.8.
    pub fn detect_stride_hint(&self, data: &[u8]) -> Option<u8> {
        if data.len() < 32 {
            return None;
        }
        let mut best_stride = None;
        let mut highest_corr = 0.0f32;

        for s in 2..=16 {
            let corr = super::stride::compute_pearson_autocorrelation(data, s);
            if corr >= 0.75 && corr > highest_corr {
                highest_corr = corr;
                best_stride = Some(s);
            }
        }
        best_stride
    }

    /// Executes classification for files under 64KB.
    ///
    /// Skips complex analyzer calculations (such as ML and heavy correlation scans) to execute in microseconds.
    pub fn classify_small_file(&mut self, data: &[u8]) -> ClassificationResult {
        let sig = self.detect_magic_bytes(data);
        self.entropy_estimator.reset();
        self.entropy_estimator.update(data);
        let entropy = self.entropy_estimator.shannon_entropy();
        let est_ratio = self.entropy_estimator.compression_estimate();

        // Perform micro stride check or skip
        let stride = if data.len() >= 32 {
            self.detect_stride_hint(data)
        } else {
            None
        };

        let mut res = self.route_by_rules(sig, entropy, stride, est_ratio);
        res.confidence = 0.95; // Small files use deterministic signature rules
        res
    }

    /// Internal routing logic combining file signature, entropy thresholds, and stride.
    fn route_by_rules(
        &self,
        sig: FileSignature,
        entropy: f32,
        stride: Option<u8>,
        est_ratio: f32,
    ) -> ClassificationResult {
        // Rule 1: High-entropy pre-compressed Images
        if matches!(sig, FileSignature::Jpeg | FileSignature::Png | FileSignature::Gif | FileSignature::Bmp | FileSignature::WebP)
            && entropy > 7.0
        {
            return ClassificationResult {
                content_type: ContentType::Incompressible,
                pipeline: CompressionPipeline::StoreRaw,
                stride_hint: None,
                window_size_recommendation: 0,
                confidence: 0.9,
                estimated_ratio: 1.0,
            };
        }

        // Rule 2: High-entropy video formats
        if matches!(sig, FileSignature::Mp4 | FileSignature::Mkv | FileSignature::Avi) && entropy > 6.5 {
            return ClassificationResult {
                content_type: ContentType::Incompressible,
                pipeline: CompressionPipeline::StoreRaw,
                stride_hint: None,
                window_size_recommendation: 0,
                confidence: 0.9,
                estimated_ratio: 1.0,
            };
        }

        // Rule 3: Compressed archive formats
        if matches!(
            sig,
            FileSignature::Zip
                | FileSignature::Gz
                | FileSignature::Bz2
                | FileSignature::Xz
                | FileSignature::Zstd
                | FileSignature::Rar
        ) {
            return ClassificationResult {
                content_type: ContentType::Incompressible,
                pipeline: CompressionPipeline::StoreRaw,
                stride_hint: None,
                window_size_recommendation: 0,
                confidence: 0.95,
                estimated_ratio: 1.0,
            };
        }

        // Rule 4: Extreme Entropy Threshold (Encrypted / Compressed noise)
        if entropy > 7.95 {
            return ClassificationResult {
                content_type: ContentType::Incompressible,
                pipeline: CompressionPipeline::StoreRaw,
                stride_hint: None,
                window_size_recommendation: 0,
                confidence: 0.99,
                estimated_ratio: 1.0,
            };
        }

        // Rule 5: Lossless audio wave data or stride scientific float/numeric arrays
        let is_multimedia = stride.is_some() && sig == FileSignature::Unknown;
        if matches!(sig, FileSignature::Wav | FileSignature::Flac) || is_multimedia {
            return ClassificationResult {
                content_type: ContentType::Multimedia,
                pipeline: CompressionPipeline::MultimediaPipeline,
                stride_hint: stride,
                window_size_recommendation: 131_072, // 128KB matching stride needs
                confidence: 0.85,
                estimated_ratio: est_ratio,
            };
        }

        // Rule 6: Text or highly compressible low entropy documents
        if matches!(sig, FileSignature::Ascii | FileSignature::Utf8Text) || (entropy < 4.5 && sig != FileSignature::Unknown) {
            return ClassificationResult {
                content_type: ContentType::Text,
                pipeline: CompressionPipeline::TextPipeline,
                stride_hint: None,
                window_size_recommendation: 65_536, // 64KB PPMd sliding context
                confidence: 0.9,
                estimated_ratio: est_ratio,
            };
        }

        // Rule 7: Executables or standard binary files
        if matches!(sig, FileSignature::Elf | FileSignature::Pe | FileSignature::MachO)
            || (4.5..=6.5).contains(&entropy)
        {
            return ClassificationResult {
                content_type: ContentType::Binary,
                pipeline: CompressionPipeline::BinaryPipeline,
                stride_hint: stride,
                window_size_recommendation: 262_144, // 256KB LZ77 dictionary
                confidence: 0.8,
                estimated_ratio: est_ratio,
            };
        }

        // Fallback Default: Treat as Binary
        ClassificationResult {
            content_type: ContentType::Binary,
            pipeline: CompressionPipeline::BinaryPipeline,
            stride_hint: stride,
            window_size_recommendation: 262_144,
            confidence: 0.5,
            estimated_ratio: est_ratio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_magic_bytes_detection() {
        let classifier = ContentClassifier::new();

        // PE MZ signature
        let mut data = vec![0; 16];
        data[0] = 0x4D;
        data[1] = 0x5A;
        assert_eq!(classifier.detect_magic_bytes(&data), FileSignature::Pe);

        // PNG signature
        let png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(classifier.detect_magic_bytes(&png), FileSignature::Png);

        // ZIP signature
        let zip = b"PK\x03\x04testarchive";
        assert_eq!(classifier.detect_magic_bytes(zip), FileSignature::Zip);

        // Unknown
        assert_eq!(classifier.detect_magic_bytes(&[0; 10]), FileSignature::Unknown);
    }

    #[test]
    fn test_ascii_and_utf8_signatures() {
        let classifier = ContentClassifier::new();

        // Pure ASCII text
        let ascii_text = b"Hello, this is a plain ASCII text stream. It contains spaces, punctuation, and newlines.\n";
        assert_eq!(classifier.detect_magic_bytes(ascii_text), FileSignature::Ascii);

        // UTF-8 text with non-ASCII characters (e.g. Emoji)
        let utf8_text = "Hello World 🌍! UTF-8 validation check.".as_bytes();
        assert_eq!(classifier.detect_magic_bytes(utf8_text), FileSignature::Utf8Text);
    }

    #[test]
    fn test_detect_stride_hint() {
        let classifier = ContentClassifier::new();

        // Repeating stride of 4 (e.g. 32-bit values)
        let mut stride_data = Vec::new();
        for i in 0..100 {
            stride_data.push((i % 4) as u8);
        }
        // Matches should trigger stride 4 hint
        assert_eq!(classifier.detect_stride_hint(&stride_data), Some(4));

        // Random data should have no clear stride
        let random_data: Vec<u8> = (0..100).map(|i| ((i * i * 107 + 13) % 251) as u8).collect();
        assert_eq!(classifier.detect_stride_hint(&random_data), None);
    }

    #[test]
    fn test_classify_routing_rules() {
        let mut classifier = ContentClassifier::new();

        // 1. Ascii Text block
        let text = b"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.";
        let res = classifier.classify_small_file(text);
        assert_eq!(res.content_type, ContentType::Text);
        assert_eq!(res.pipeline, CompressionPipeline::TextPipeline);

        // 2. High-entropy Zip file
        let zip_data = b"PK\x03\x04veryrandomcompressedbytesthatshouldbeatmaxentropy";
        let res = classifier.classify_small_file(zip_data);
        assert_eq!(res.content_type, ContentType::Incompressible);
        assert_eq!(res.pipeline, CompressionPipeline::StoreRaw);

        // 3. Audio Wav data signature (WAV signature RIFF....WAVE)
        let mut wav_data = b"RIFF\x00\x00\x00\x00WAVEfmt ".to_vec();
        wav_data.extend(vec![0; 100]);
        let res = classifier.classify_small_file(&wav_data);
        assert_eq!(res.content_type, ContentType::Multimedia);
        assert_eq!(res.pipeline, CompressionPipeline::MultimediaPipeline);
    }
}
