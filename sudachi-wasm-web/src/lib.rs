use serde::Serialize;
use sudachi::analysis::stateful_tokenizer::StatefulTokenizer;
use sudachi::analysis::Mode;
use sudachi::dic::character_category::CharacterCategory;
use sudachi::dic::LoadedDictionary;
use sudachi::prelude::*;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Morpheme {
    surface: String,
    reading_form: String,
    dictionary_form: String,
    normalized_form: String,
    part_of_speech: Vec<String>,
    is_oov: bool,
    begin: usize,
    end: usize,
}

#[wasm_bindgen]
pub struct Tokenizer {
    dict: LoadedDictionary<'static>,
}

#[wasm_bindgen]
impl Tokenizer {
    #[wasm_bindgen(constructor)]
    pub fn new(dict_bytes: &[u8]) -> Result<Tokenizer, JsValue> {
        // Leak the bytes so we get a 'static slice (WASM is single-threaded, no drop needed)
        let bytes: &'static [u8] = Box::leak(dict_bytes.to_vec().into_boxed_slice());
        let char_cat = CharacterCategory::from_bytes(
            include_bytes!("../../resources/char.def"),
        )
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let dict = LoadedDictionary::from_system_dictionary_and_chardef(bytes, char_cat)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Tokenizer { dict })
    }

    /// Load from a .xdic compressed dictionary (xcdat trie + chunk-zstd word infos).
    /// chunk_size must match the value used during compression (default 65536).
    #[wasm_bindgen(js_name = fromXdic)]
    pub fn from_xdic(dict_bytes: &[u8], chunk_size: usize) -> Result<Tokenizer, JsValue> {
        let bytes: &'static [u8] = Box::leak(dict_bytes.to_vec().into_boxed_slice());
        let char_cat = CharacterCategory::from_bytes(
            include_bytes!("../../resources/char.def"),
        )
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let dict = LoadedDictionary::from_xdic(bytes, char_cat, chunk_size)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Tokenizer { dict })
    }

    pub fn tokenize(&self, text: &str, mode: &str) -> Result<JsValue, JsValue> {
        let mode = match mode {
            "A" => Mode::A,
            "B" => Mode::B,
            _ => Mode::C,
        };
        let mut tok = StatefulTokenizer::create(&self.dict, false, mode);
        let mut morphemes = MorphemeList::empty(&self.dict);
        tok.reset().push_str(text);
        tok.do_tokenize().map_err(|e| JsValue::from_str(&e.to_string()))?;
        morphemes.collect_results(&mut tok).map_err(|e| JsValue::from_str(&e.to_string()))?;

        let result: Vec<Morpheme> = morphemes
            .iter()
            .map(|m| Morpheme {
                surface: m.surface().to_string(),
                reading_form: m.reading_form().to_string(),
                dictionary_form: m.dictionary_form().to_string(),
                normalized_form: m.normalized_form().to_string(),
                part_of_speech: m.part_of_speech().to_vec(),
                is_oov: m.is_oov(),
                begin: m.begin(),
                end: m.end(),
            })
            .collect();

        serde_wasm_bindgen::to_value(&result).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
