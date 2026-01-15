// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Replacement for ICU library bindings using native Rust.
//! Includes a "Full" mode using the regex crate, and a "Lite" mode using standard string search.

use std::cmp::Ordering;
use std::mem::MaybeUninit;
use std::ops::Range;

use stdext::arena::{Arena, ArenaString};

use crate::buffer::TextBuffer;
use crate::apperr;

#[derive(Clone, Copy)]
pub struct Encoding {
    pub label: &'static str,
    pub canonical: &'static str,
}

pub struct Encodings {
    pub preferred: &'static [Encoding],
    pub all: &'static [Encoding],
}

static ENCODINGS: Encodings = Encodings {
    preferred: &[
        Encoding { label: "UTF-8", canonical: "UTF-8" },
        Encoding { label: "UTF-8 BOM", canonical: "UTF-8 BOM" },
    ],
    all: &[
        Encoding { label: "UTF-8", canonical: "UTF-8" },
        Encoding { label: "UTF-8 BOM", canonical: "UTF-8 BOM" },
    ],
};

pub fn get_available_encodings() -> &'static Encodings {
    &ENCODINGS
}

pub fn apperr_format(f: &mut std::fmt::Formatter<'_>, code: u32) -> std::fmt::Result {
    write!(f, "ICU Error (Stub): {code:#08x}")
}

pub fn init() -> apperr::Result<()> {
    Ok(())
}

pub struct Converter<'pivot> {
    _marker: std::marker::PhantomData<&'pivot mut [MaybeUninit<u16>]>,
}

impl<'pivot> Converter<'pivot> {
    pub fn new(
        _pivot_buffer: &'pivot mut [MaybeUninit<u16>],
        source_encoding: &str,
        target_encoding: &str,
    ) -> apperr::Result<Self> {
        if (source_encoding == "UTF-8" || source_encoding == "UTF-8 BOM") &&
           (target_encoding == "UTF-8" || target_encoding == "UTF-8 BOM") {
            Ok(Self { _marker: std::marker::PhantomData })
        } else {
            Err(apperr::Error::new_icu(16))
        }
    }

    pub fn convert(
        &mut self,
        input: &[u8],
        output: &mut [MaybeUninit<u8>],
    ) -> apperr::Result<(usize, usize)> {
        let len = input.len().min(output.len());
        unsafe {
            std::ptr::copy_nonoverlapping(input.as_ptr(), output.as_mut_ptr() as *mut u8, len);
        }
        Ok((len, len))
    }
}

pub fn compare_strings(a: &[u8], b: &[u8]) -> Ordering {
    a.cmp(b)
}

pub fn fold_case<'a>(arena: &'a Arena, input: &str) -> ArenaString<'a> {
    let folded = input.to_lowercase();
    ArenaString::from_str(arena, &folded)
}

// -----------------------------------------------------------------------------------------
// Regex and Text implementation (Shared Logic)
// -----------------------------------------------------------------------------------------

pub struct Text {
    pub content: String,
    tb_ptr: *const TextBuffer,
}

impl Drop for Text {
    fn drop(&mut self) {}
}

impl Text {
    pub unsafe fn new(tb: &TextBuffer) -> apperr::Result<Self> {
        let mut t = Self { 
            content: String::new(), 
            tb_ptr: tb as *const _ 
        };
        t.refresh();
        Ok(t)
    }

    pub unsafe fn refresh(&mut self) {
        let tb = &*self.tb_ptr;
        self.content.clear();
        self.content.reserve(tb.text_length());
        
        let mut offset = 0;
        loop {
            let chunk = tb.read_forward(offset);
            if chunk.is_empty() {
                break;
            }
            self.content.push_str(&String::from_utf8_lossy(chunk));
            offset += chunk.len();
        }
    }
}

// -----------------------------------------------------------------------------------------
// Implementation 1: FULL MODE (Using regex crate)
// -----------------------------------------------------------------------------------------
#[cfg(feature = "regex")]
pub struct Regex {
    inner: regex::Regex,
    text: String,
    last_idx: usize,
    captures: Option<Vec<Range<usize>>>,
}

#[cfg(feature = "regex")]
impl Regex {
    pub const CASE_INSENSITIVE: i32 = 1;
    pub const MULTILINE: i32 = 2;
    pub const LITERAL: i32 = 4;

    pub unsafe fn new(pattern: &str, flags: i32, text: &Text) -> apperr::Result<Self> {
        let pattern_string;
        let final_pattern = if (flags & Self::LITERAL) != 0 {
            pattern_string = regex::escape(pattern);
            &pattern_string
        } else {
            pattern
        };

        let mut builder = regex::RegexBuilder::new(final_pattern);
        
        if (flags & Self::CASE_INSENSITIVE) != 0 {
            builder.case_insensitive(true);
        }
        if (flags & Self::MULTILINE) != 0 {
            builder.multi_line(true);
        }
        
        match builder.build() {
            Ok(inner) => Ok(Self {
                inner,
                text: text.content.clone(),
                last_idx: 0,
                captures: None,
            }),
            Err(_) => Err(apperr::Error::new_icu(1)),
        }
    }

    pub unsafe fn set_text(&mut self, text: &mut Text, offset: usize) {
        text.refresh();
        self.text = text.content.clone();
        self.reset(offset);
    }

    pub fn reset(&mut self, offset: usize) {
        self.last_idx = offset;
        self.captures = None;
    }

    pub fn group_count(&mut self) -> i32 {
        if let Some(caps) = &self.captures {
            (caps.len() as i32).saturating_sub(1)
        } else {
            0
        }
    }

    pub fn group(&mut self, group: i32) -> Option<Range<usize>> {
        if let Some(caps) = &self.captures {
            caps.get(group as usize).cloned()
        } else {
            None
        }
    }
}

#[cfg(feature = "regex")]
impl Iterator for Regex {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.last_idx > self.text.len() {
            return None;
        }

        match self.inner.captures_at(&self.text, self.last_idx) {
            Some(caps) => {
                let m = caps.get(0).unwrap();
                let range = m.start()..m.end();
                
                let mut groups = Vec::new();
                for i in 0..caps.len() {
                    if let Some(g) = caps.get(i) {
                        groups.push(g.start()..g.end());
                    } else {
                        groups.push(0..0);
                    }
                }
                self.captures = Some(groups);
                
                if range.start == range.end {
                     self.last_idx = range.end + 1;
                } else {
                     self.last_idx = range.end;
                }

                Some(range)
            }
            None => None,
        }
    }
}

// -----------------------------------------------------------------------------------------
// Implementation 2: LITE MODE (Using std string search)
// -----------------------------------------------------------------------------------------
#[cfg(not(feature = "regex"))]
pub struct Regex {
    pattern: String,
    text: String,
    last_idx: usize,
    case_insensitive: bool,
    whole_word: bool,
}

#[cfg(not(feature = "regex"))]
impl Regex {
    pub const CASE_INSENSITIVE: i32 = 1;
    pub const MULTILINE: i32 = 2; // Ignored in lite
    pub const LITERAL: i32 = 4;   // Always literal in lite

    pub unsafe fn new(pattern: &str, flags: i32, text: &Text) -> apperr::Result<Self> {
        let mut p = pattern;
        let mut whole_word = false;

        // Detect if the pattern was wrapped in \b by the buffer logic for whole word search.
        // Since Lite mode doesn't support regex, we strip it and handle logic manually.
        if p.starts_with(r"\b") && p.ends_with(r"\b") && p.len() >= 4 {
             p = &p[2..p.len()-2];
             whole_word = true;
        }

        Ok(Self {
            pattern: p.to_string(),
            text: text.content.clone(),
            last_idx: 0,
            case_insensitive: (flags & Self::CASE_INSENSITIVE) != 0,
            whole_word,
        })
    }

    pub unsafe fn set_text(&mut self, text: &mut Text, offset: usize) {
        text.refresh();
        self.text = text.content.clone();
        self.reset(offset);
    }

    pub fn reset(&mut self, offset: usize) {
        self.last_idx = offset;
    }

    pub fn group_count(&mut self) -> i32 { 0 }

    pub fn group(&mut self, _group: i32) -> Option<Range<usize>> { None }
    
    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
}

#[cfg(not(feature = "regex"))]
impl Iterator for Regex {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.last_idx > self.text.len() {
            return None;
        }

        let slice = &self.text[self.last_idx..];
        
        // Native search logic
        if self.case_insensitive {
            // Optimization: iterate slice chars instead of allocating lowercased string.
            // This is O(N*M) in worst case but avoids the massive O(N) allocation per search.
            // 1. Prepare pattern: simple lowercase.
            let pat_lower: Vec<char> = self.pattern.to_lowercase().chars().collect();
            if pat_lower.is_empty() {
                return Some(self.last_idx..self.last_idx);
            }

            // 2. Scan text
            for (offset, _) in slice.char_indices() {
                let mut sub_iter = slice[offset..].chars();
                let mut pat_iter = pat_lower.iter();
                let mut current_match_len = 0;
                
                let matches = loop {
                    match pat_iter.next() {
                        Some(&p_char) => {
                            match sub_iter.next() {
                                Some(t_char) => {
                                    // Compare t_char lowercased with p_char.
                                    let mut t_lower = t_char.to_lowercase();
                                    if let Some(tl) = t_lower.next() {
                                        if tl != p_char {
                                            break false;
                                        }
                                    } else {
                                        break false;
                                    }
                                    current_match_len += t_char.len_utf8();
                                }
                                None => break false, // Text ended before pattern
                            }
                        }
                        None => break true, // Pattern exhausted -> Match found
                    }
                };

                if matches {
                    let start = self.last_idx + offset;
                    let end = start + current_match_len;
                    
                    // Whole word check
                    if self.whole_word {
                        let prev_char = if start > 0 {
                            self.text[..start].chars().next_back()
                        } else {
                            None
                        };
                        let next_char = self.text[end..].chars().next();
                        
                        if prev_char.map_or(false, Self::is_word_char) || next_char.map_or(false, Self::is_word_char) {
                            continue; // Not a whole word match, skip
                        }
                    }

                    self.last_idx = end;
                    return Some(start..end);
                }
            }
            None
        } else {
            // Case sensitive search
            let mut search_offset = 0;
            loop {
                let sub_slice = &slice[search_offset..];
                match sub_slice.find(&self.pattern) {
                    Some(idx) => {
                        let match_start_in_slice = search_offset + idx;
                        let start = self.last_idx + match_start_in_slice;
                        let end = start + self.pattern.len();
                        
                        // Whole word check
                        if self.whole_word {
                            let prev_char = if start > 0 {
                                self.text[..start].chars().next_back()
                            } else {
                                None
                            };
                            let next_char = self.text[end..].chars().next();
                            
                            if prev_char.map_or(false, Self::is_word_char) || next_char.map_or(false, Self::is_word_char) {
                                // Not a whole word, continue searching in the rest of the slice
                                search_offset += idx + 1; // Move past this partial match
                                continue;
                            }
                        }

                        self.last_idx = end;
                        return Some(start..end);
                    }
                    None => return None,
                }
            }
        }
    }
}
