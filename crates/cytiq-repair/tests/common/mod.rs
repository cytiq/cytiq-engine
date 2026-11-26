//! Synthetic FCS files with the deviations of real ones.

#![allow(dead_code)]

/// Two `f32` channels and three events, and how the file deviates.
#[derive(Clone)]
pub struct Spec {
    pub version: &'static str,
    pub delimiter: u8,
    /// Where TEXT starts; HEADER's 58 bytes are blank up to there.
    pub text_start: usize,
    /// Keywords after the standard ones.
    pub keywords: Vec<(&'static str, &'static str)>,
    /// Bytes of TEXT after the pairs, as they are.
    pub raw_text: Vec<u8>,
    /// Bytes of TEXT after everything else.
    pub padding: Vec<u8>,
    /// Bytes between TEXT and DATA.
    pub gap: Vec<u8>,
    /// Bytes of DATA added to its declared end.
    pub data_end_past: u64,
    /// DATA starts at the last byte HEADER counts to TEXT.
    pub overlap: bool,
    pub analysis: Option<Vec<u8>>,
    /// Bytes after the last segment.
    pub tail: Vec<u8>,
    /// HEADER offsets to write instead: TEXT, DATA, ANALYSIS.
    pub header: [Option<(i64, i64)>; 3],
    /// Keyword values to write instead.
    pub overrides: Vec<(&'static str, String)>,
    /// Supplemental TEXT offsets: the whole of TEXT, or its first delimiter.
    pub supplemental: Option<bool>,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            version: "FCS3.1",
            delimiter: b'/',
            text_start: 58,
            keywords: vec![("$CYT", "Model//X"), ("VENDOR", "kept")],
            raw_text: Vec::new(),
            padding: Vec::new(),
            gap: Vec::new(),
            data_end_past: 0,
            overlap: false,
            analysis: None,
            tail: Vec::new(),
            header: [None; 3],
            overrides: Vec::new(),
            supplemental: None,
        }
    }
}

pub const EVENTS: [[f32; 2]; 3] = [[1.0, -2.5], [3.0, 4.5], [-0.0, f32::MAX]];

/// The bytes of DATA.
pub fn data() -> Vec<u8> {
    EVENTS
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

impl Spec {
    fn text(&self, offsets: &[(&str, u64)]) -> Vec<u8> {
        let mut pairs: Vec<(String, String)> = [
            ("$MODE", "L"),
            ("$DATATYPE", "F"),
            ("$BYTEORD", "1,2,3,4"),
            ("$NEXTDATA", "0"),
            ("$PAR", "2"),
            ("$TOT", "3"),
            ("$P1N", "FSC-A"),
            ("$P1B", "32"),
            ("$P1E", "0,0"),
            ("$P1R", "1024"),
            ("$P2N", "FL1-A"),
            ("$P2B", "32"),
            ("$P2E", "0,0"),
            ("$P2R", "1024"),
        ]
        .iter()
        .chain(&self.keywords)
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
        for (key, value) in offsets {
            pairs.push(((*key).to_owned(), format!("{value:08}")));
        }
        for (key, value) in &self.overrides {
            match pairs.iter_mut().find(|(found, _)| found == key) {
                Some(pair) => pair.1.clone_from(value),
                None => pairs.push(((*key).to_owned(), value.clone())),
            }
        }
        let mut text = vec![self.delimiter];
        for (key, value) in pairs.iter().filter(|(_, value)| value != "<absent>") {
            for token in [key, value] {
                // Values are written as given: a doubled delimiter is doubled.
                text.extend(
                    token
                        .replace('/', &char::from(self.delimiter).to_string())
                        .bytes(),
                );
                text.push(self.delimiter);
            }
        }
        text.extend_from_slice(&self.raw_text);
        text.extend_from_slice(&self.padding);
        text
    }

    pub fn build(&self) -> Vec<u8> {
        let data = data();
        let names = [
            "$BEGINDATA",
            "$ENDDATA",
            "$BEGINSTEXT",
            "$ENDSTEXT",
            "$BEGINANALYSIS",
            "$ENDANALYSIS",
        ];
        let zeros: Vec<(&str, u64)> = names.iter().map(|name| (*name, 0)).collect();
        let length = self.text(&zeros).len() as u64;
        let start = self.text_start as u64;
        // With an overlap HEADER counts the first byte of DATA to TEXT.
        let text_end = start + length - 1 + u64::from(self.overlap);
        let data_start = start + length + self.gap.len() as u64;
        let data_end = data_start + data.len() as u64 - 1;
        let analysis = self
            .analysis
            .as_ref()
            .map(|bytes| (data_end + 1, data_end + bytes.len() as u64));
        let supplemental = match self.supplemental {
            None => (0, 0),
            Some(true) => (start, text_end),
            Some(false) => (start, start),
        };
        let (analysis_start, analysis_end) = analysis.unwrap_or((0, 0));
        let values = [
            data_start,
            data_end + self.data_end_past,
            supplemental.0,
            supplemental.1,
            analysis_start,
            analysis_end,
        ];
        let offsets: Vec<(&str, u64)> = names.iter().copied().zip(values).collect();
        let text = self.text(&offsets);
        assert_eq!(text.len() as u64, length);
        let signed =
            |(start, end): (u64, u64)| (i64::try_from(start).unwrap(), i64::try_from(end).unwrap());
        let declared = [
            signed((start, text_end)),
            signed((data_start, data_end + self.data_end_past)),
            signed((analysis_start, analysis_end)),
        ];
        let mut file = format!("{}    ", self.version).into_bytes();
        for (declared, instead) in declared.into_iter().zip(self.header) {
            let (start, end) = instead.unwrap_or(declared);
            file.extend(format!("{start:>8}{end:>8}").into_bytes());
        }
        file.resize(self.text_start, b' ');
        file.extend(text);
        file.extend_from_slice(&self.gap);
        file.extend(data);
        file.extend(self.analysis.iter().flatten());
        file.extend_from_slice(&self.tail);
        file
    }
}
