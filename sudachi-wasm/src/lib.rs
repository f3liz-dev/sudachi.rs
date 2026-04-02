#[path = "sudachi.rs"]
mod bindings;

use bindings::export;
use bindings::exports::sudachi::tokenizer::api::Guest;

use sudachi::analysis::stateful_tokenizer::StatefulTokenizer;
use sudachi::analysis::Mode;
use sudachi::dic::character_category::CharacterCategory;
use sudachi::dic::LoadedDictionary;
use sudachi::prelude::*;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

const DEFAULT_CHAR_DEF: &[u8] = include_bytes!("../../resources/char.def");
const CHUNK_SIZE: usize = 65536;

static NEXT_HANDLE: AtomicU32 = AtomicU32::new(1);

thread_local! {
    static DICTS: RefCell<HashMap<u32, LoadedDictionary<'static>>> = RefCell::new(HashMap::new());
}

struct SudachiComponent;

impl Guest for SudachiComponent {
    fn load_dictionary(xdic_bytes: Vec<u8>) -> Result<u32, String> {
        let bytes: &'static [u8] = Box::leak(xdic_bytes.into_boxed_slice());
        let char_cat = CharacterCategory::from_bytes(DEFAULT_CHAR_DEF)
            .map_err(|e| e.to_string())?;
        let dict = LoadedDictionary::from_xdic(bytes, char_cat, CHUNK_SIZE)
            .map_err(|e| e.to_string())?;
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        DICTS.with(|d| d.borrow_mut().insert(handle, dict));
        Ok(handle)
    }

    fn tokenize(handle: u32, text: String, mode: u8) -> Result<Vec<(String, String, String)>, String> {
        let mode = match mode { 0 => Mode::A, 1 => Mode::B, _ => Mode::C };
        DICTS.with(|d| {
            let dicts = d.borrow();
            let dict = dicts.get(&handle).ok_or("invalid handle")?;
            let mut tok = StatefulTokenizer::create(dict, false, mode);
            let mut morphemes = MorphemeList::empty(dict);
            tok.reset().push_str(&text);
            tok.do_tokenize().map_err(|e| e.to_string())?;
            morphemes.collect_results(&mut tok).map_err(|e| e.to_string())?;
            Ok(morphemes.iter().map(|m| (
                m.surface().to_string(),
                m.reading_form().to_string(),
                m.part_of_speech().first().map(|s| s.as_str()).unwrap_or("").to_string(),
            )).collect())
        })
    }

    fn free_dictionary(handle: u32) {
        DICTS.with(|d| d.borrow_mut().remove(&handle));
    }
}

export!(SudachiComponent with_types_in bindings);
