/*
 *  Copyright (c) 2021 Works Applications Co., Ltd.
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

extern crate lazy_static;
extern crate sudachi;

use std::ops::Deref;
use sudachi::prelude::Mode;

mod common;
use crate::common::TestStatefulTokenizer as TestTokenizer;
use common::LEX_CSV;

#[test]
fn empty() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("");
    assert_eq!(0, ms.len());
}

#[test]
fn tokenize_small_katakana_only() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("ァ");
    assert_eq!(1, ms.len());
}

#[test]
fn get_word_id() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("京都");
    assert_eq!(1, ms.len());
    let m0 = ms.get(0);
    let pos = m0.part_of_speech();
    assert_eq!(&["名詞", "固有名詞", "地名", "一般", "*", "*"], pos);

    // we do not have word_id field in Morpheme and skip testing.
    let ms = tok.tokenize("ぴらる");
    assert_eq!(1, ms.len());
    let m0 = ms.get(0);
    let pos = m0.part_of_speech();
    assert_eq!(&["名詞", "普通名詞", "一般", "*", "*", "*"], pos);
}

#[test]
fn get_dictionary_id() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("京都");
    assert_eq!(1, ms.len());
    assert_eq!(0, ms.get(0).dictionary_id());

    let ms = tok.tokenize("ぴらる");
    assert_eq!(1, ms.len());
    assert_eq!(1, ms.get(0).dictionary_id());

    let ms = tok.tokenize("京");
    assert_eq!(1, ms.len());
    assert!(ms.get(0).dictionary_id() < 0);
}

#[test]
fn get_synonym_group_id() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("京都");
    assert_eq!(1, ms.len());
    assert_eq!([1, 5], ms.get(0).synonym_group_ids());

    let ms = tok.tokenize("ぴらる");
    assert_eq!(1, ms.len());
    assert!(ms.get(0).synonym_group_ids().is_empty());

    let ms = tok.tokenize("東京府");
    assert_eq!(1, ms.len());
    assert_eq!([1, 3], ms.get(0).synonym_group_ids());
}

#[test]
fn tokenize_kanji_alphabet_word() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    assert_eq!(1, tok.tokenize("特a").len());
    assert_eq!(1, tok.tokenize("ab").len());
    assert_eq!(2, tok.tokenize("特ab").len());
}

#[test]
fn tokenize_with_dots() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("京都…");
    assert_eq!(4, ms.len());
    assert_eq!("…", ms.get(1).surface().deref());
    assert_eq!(".", ms.get(1).normalized_form());
    assert_eq!("", ms.get(2).surface().deref());
    assert_eq!(".", ms.get(2).normalized_form());
    assert_eq!("", ms.get(3).surface().deref());
    assert_eq!(".", ms.get(3).normalized_form());
}

#[test]
fn tokenizer_morpheme_split() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("東京都");
    assert_eq!(1, ms.len());
    assert_eq!("東京都", ms.get(0).surface().deref());

    tok.set_mode(Mode::A);
    let ms = tok.tokenize("東京都");
    assert_eq!(2, ms.len());
    assert_eq!("東京", ms.get(0).surface().deref());
    assert_eq!("都", ms.get(1).surface().deref());
}

#[test]
fn split_middle() {
    let mut tok = TestTokenizer::new_built(Mode::C);
    let ms = tok.tokenize("京都東京都京都");
    assert_eq!(ms.len(), 3);
    let m = ms.get(1);
    assert_eq!(m.surface().deref(), "東京都");

    let mut ms_a = ms.empty_clone();
    assert!(m.split_into(Mode::A, &mut ms_a).expect("works"));
    assert_eq!(ms_a.len(), 2);
    assert_eq!(ms_a.get(0).surface().deref(), "東京");
    assert_eq!(ms_a.get(0).begin_c(), 2);
    assert_eq!(ms_a.get(0).end_c(), 4);
    assert_eq!(ms_a.get(0).begin(), 6);
    assert_eq!(ms_a.get(0).end(), 12);
    assert_eq!(ms_a.get(1).surface().deref(), "都");
    assert_eq!(ms_a.get(1).begin_c(), 4);
    assert_eq!(ms_a.get(1).end_c(), 5);
    assert_eq!(ms_a.get(1).begin(), 12);
    assert_eq!(ms_a.get(1).end(), 15);
}

const OOV_CFG: &[u8] = include_bytes!("resources/sudachi.oov.json");

#[test]
fn istanbul_is_not_splitted() {
    let mut tok = TestTokenizer::builder(LEX_CSV).config(OOV_CFG).build();
    let ms = tok.tokenize("İstanbul");
    assert_eq!(ms.len(), 1);
}

#[test]
fn emoji_are_not_splitted() {
    let mut tok = TestTokenizer::builder(LEX_CSV).config(OOV_CFG).build();
    assert_eq!(tok.tokenize("⏸").len(), 1);
    assert_eq!(tok.tokenize("🦹‍♂️").len(), 1);
    assert_eq!(tok.tokenize("🎅🏾").len(), 1);
    assert_eq!(tok.tokenize("👳🏽‍♂").len(), 1);
}

#[test]
fn zeros_are_accepted() {
    let mut tok = TestTokenizer::builder(LEX_CSV).config(OOV_CFG).build();
    let ms = tok.tokenize("京都\0いく");
    assert_eq!(ms.len(), 3);
    assert_eq!(ms.get(0).surface().deref(), "京都");
    assert_eq!(ms.get(1).surface().deref(), "\0");
    assert_eq!(ms.get(2).surface().deref(), "いく");

    let ms = tok.tokenize("\0京都いく");
    assert_eq!(ms.len(), 3);
    assert_eq!(ms.get(0).surface().deref(), "\0");
    assert_eq!(ms.get(1).surface().deref(), "京都");
    assert_eq!(ms.get(2).surface().deref(), "いく");
}

#[test]
fn morpheme_extraction() {
    let mut tok = TestTokenizer::builder(LEX_CSV).config(OOV_CFG).build();
    let entries = tok.entries("東京都");
    assert_eq!(1, entries.len());
    let e = entries.get(0);
    assert_eq!("東京都", e.surface().deref());
    assert_eq!(0, e.begin());
    assert_eq!(9, e.end());
    assert_eq!(0, e.begin_c());
    assert_eq!(3, e.end_c());
}

#[test]
fn xdic_tokenize_readings() {
    use sudachi::dic::character_category::CharacterCategory;
    use sudachi::dic::LoadedDictionary;
    use sudachi::analysis::stateful_tokenizer::StatefulTokenizer;
    use sudachi::prelude::MorphemeList;

    let xdic = std::fs::read("../../demo/system_core.xdic").expect("read system_core.xdic");
    let char_def = std::fs::read("../resources/char.def").expect("read char.def");
    let dict = LoadedDictionary::from_xdic(&xdic, CharacterCategory::from_bytes(&char_def).unwrap(), 65536)
        .expect("from_xdic failed");
    println!("pos_list len: {}", dict.grammar.pos_list.len());
    println!("connect matrix: {}x{}", dict.grammar.conn_matrix().num_left(), dict.grammar.conn_matrix().num_right());

    let mut tok = StatefulTokenizer::create(&dict, false, Mode::C);
    let mut ms = MorphemeList::empty(&dict);
    tok.reset().push_str("今日はいい天気ですね。");
    tok.do_tokenize().unwrap();
    ms.collect_results(&mut tok).unwrap();

    for m in ms.iter() { println!("{}\t{}", m.surface(), m.reading_form()); }

    let m0 = ms.get(0);
    let r: &str = &m0.reading_form();
    assert!(r == "キョウ" || r == "コンニチ", "got {r}");
}

#[test]
fn xcdat_lookup_kyou() {
    use sudachi::dic::lexicon::xcdat_trie::XcdatTrie;
    let xdic = std::fs::read("../../demo/system_core.xdic").expect("xdic");
    let pos = 272usize;
    let mut p = pos;
    let grammar_len = u32::from_le_bytes(xdic[p..p+4].try_into().unwrap()) as usize; p += 4 + grammar_len;
    let xcdat_len = u32::from_le_bytes(xdic[p..p+4].try_into().unwrap()) as usize; p += 4;
    let xcdat_bytes = &xdic[p..p+xcdat_len]; p += xcdat_len;
    let r2o_count = u32::from_le_bytes(xdic[p..p+4].try_into().unwrap()) as usize; p += 4;
    let rank_to_offset: Vec<u32> = (0..r2o_count).map(|i| u32::from_le_bytes(xdic[p+i*4..p+i*4+4].try_into().unwrap())).collect();

    let trie = XcdatTrie::parse(xcdat_bytes, rank_to_offset).unwrap();
    let hits: Vec<_> = trie.common_prefix_search("今日".as_bytes(), 0).collect();
    let kyou = hits.iter().find(|e| e.end == 6).expect("今日 not found");
    let ima  = hits.iter().find(|e| e.end == 3).expect("今 not found");
    assert_ne!(kyou.value, ima.value, "今日 and 今 must have different word_id_table offsets");
}
