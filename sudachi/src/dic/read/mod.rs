/*
 *  Copyright (c) 2021-2024 Works Applications Co., Ltd.
 *
 *  Licensed under the Apache License, Version 2.0 (the "License");
 *  you may not use this file except in compliance with the License.
 *  You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 *   Unless required by applicable law or agreed to in writing, software
 *  distributed under the License is distributed on an "AS IS" BASIS,
 *  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *  See the License for the specific language governing permissions and
 *  limitations under the License.
 */

pub(crate) mod u16str;
pub(crate) mod word_info;

use nom::number::complete::{le_u32, le_u8};
use nom::Parser;

use crate::dic::compact;
use crate::dic::word_id::WordId;
use crate::error::SudachiNomResult;

pub fn u32_array_parser(input: &[u8]) -> SudachiNomResult<&[u8], Vec<u32>> {
    let (rest, length) = le_u8(input)?;
    nom::multi::count(le_u32, length as usize)(rest)
}

pub fn u32_wid_array_parser(input: &[u8]) -> SudachiNomResult<&[u8], Vec<WordId>> {
    let (rest, length) = le_u8(input)?;
    nom::multi::count(le_u32.map(WordId::from_raw), length as usize)(rest)
}

pub fn skip_wid_array(input: &[u8]) -> SudachiNomResult<&[u8], Vec<WordId>> {
    let (rest, length) = le_u8(input)?;
    let num_bytes = length as usize * 4;
    let next = &rest[num_bytes..];
    Ok((next, Vec::new()))
}

pub fn skip_u32_array(input: &[u8]) -> SudachiNomResult<&[u8], Vec<u32>> {
    let (rest, length) = le_u8(input)?;
    let num_bytes = length as usize * 4;
    let next = &rest[num_bytes..];
    Ok((next, Vec::new()))
}

pub fn u32_parser(input: &[u8]) -> SudachiNomResult<&[u8], u32> {
    le_u32(input)
}

// ── VByte nom-compatible parsers ────────────────────────────────────────

/// Parse a VByte-encoded u32.
pub fn vbyte_u32(input: &[u8]) -> SudachiNomResult<&[u8], u32> {
    let (val, consumed) = compact::decode_vbyte(input);
    Ok((&input[consumed..], val))
}

/// Parse a VByte-encoded u16.
pub fn vbyte_u16(input: &[u8]) -> SudachiNomResult<&[u8], u16> {
    let (val, consumed) = compact::decode_vbyte(input);
    Ok((&input[consumed..], val as u16))
}

/// Parse a ZigZag + VByte encoded i32.
pub fn zigzag_vbyte_i32(input: &[u8]) -> SudachiNomResult<&[u8], i32> {
    let (val, consumed) = compact::decode_vbyte(input);
    Ok((&input[consumed..], compact::decode_zigzag(val)))
}

/// Parse a VByte-encoded array of u32 values.
/// Format: [vbyte count][vbyte value × count]
pub fn vbyte_u32_array(input: &[u8]) -> SudachiNomResult<&[u8], Vec<u32>> {
    let (rest, count) = vbyte_u32(input)?;
    let mut result = Vec::with_capacity(count as usize);
    let mut current = rest;
    for _ in 0..count {
        let (next, val) = vbyte_u32(current)?;
        result.push(val);
        current = next;
    }
    Ok((current, result))
}

/// Parse a VByte-encoded array of WordId values.
/// Format: [vbyte count][vbyte raw_word_id × count]
pub fn vbyte_wid_array(input: &[u8]) -> SudachiNomResult<&[u8], Vec<WordId>> {
    let (rest, count) = vbyte_u32(input)?;
    let mut result = Vec::with_capacity(count as usize);
    let mut current = rest;
    for _ in 0..count {
        let (next, val) = vbyte_u32(current)?;
        result.push(WordId::from_raw(val));
        current = next;
    }
    Ok((current, result))
}

/// Skip a VByte-encoded array (read count, skip all elements).
pub fn skip_vbyte_wid_array(input: &[u8]) -> SudachiNomResult<&[u8], Vec<WordId>> {
    let (rest, count) = vbyte_u32(input)?;
    let mut current = rest;
    for _ in 0..count {
        let (_, consumed) = compact::decode_vbyte(current);
        current = &current[consumed..];
    }
    Ok((current, Vec::new()))
}

/// Skip a VByte-encoded u32 array.
pub fn skip_vbyte_u32_array(input: &[u8]) -> SudachiNomResult<&[u8], Vec<u32>> {
    let (rest, count) = vbyte_u32(input)?;
    let mut current = rest;
    for _ in 0..count {
        let (_, consumed) = compact::decode_vbyte(current);
        current = &current[consumed..];
    }
    Ok((current, Vec::new()))
}
