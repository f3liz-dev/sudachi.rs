/// Produces a compressed sudachi dictionary (.xdic):
/// - Trie: yada → xcdat
/// - Grammar: single zstd block with trained dictionary (decompressed once at load)
/// - WordInfos offset table: uncompressed (random access index)
/// - WordInfos records: chunk-based zstd with shared dictionary
///   → only the needed chunk is decompressed per lookup
use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use memmap2::Mmap;
use sudachi::dic::header::Header;
use sudachi::dic::subset::InfoSubset;
use sudachi::dic::DictionaryLoader;

/// Chunk size for WordInfos records (bytes). 64KB balances RAM vs compression ratio.
const CHUNK_SIZE: usize = 64 * 1024;
/// Number of samples for zstd dictionary training
const DICT_TRAIN_SAMPLES: usize = 10_000;
/// zstd dictionary size
const ZDICT_SIZE: usize = 112 * 1024;

#[derive(Parser)]
#[command(about = "Repack sudachi dict: xcdat trie + chunk-zstd grammar/wordinfos")]
struct Cli {
    dict: PathBuf,
    #[arg(long)]
    sizes: bool,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(short = 'b', long, env = "XCDAT_BUILD_BIN", default_value = "xcdat_build")]
    xcdat_build: PathBuf,
    #[arg(short = 'e', long, env = "XCDAT_ENUMERATE_BIN", default_value = "xcdat_enumerate")]
    xcdat_enumerate: PathBuf,
    #[arg(short = 'l', long, default_value = "19")]
    level: i32,
    #[arg(short = 'c', long, default_value_t = CHUNK_SIZE)]
    chunk_size: usize,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let file = std::fs::File::open(&cli.dict)?;
    let data = unsafe { Mmap::map(&file) }?;
    let loader = unsafe { DictionaryLoader::read_any_dictionary(&data) }?;

    let total = data.len();
    let lex_off = loader.lexicon_offset;
    let trie_sz = loader.lexicon.trie_storage_size();
    let wi_off = loader.word_infos_offset;
    let word_count = loader.lexicon.size() as usize;

    let grammar_sz = lex_off - Header::STORAGE_SIZE;
    let wi_index_sz = word_count * 4;
    let wi_records_sz = total - wi_off - wi_index_sz;
    let rest_sz = total - lex_off - trie_sz; // word_id_table + word_params + word_infos

    println!("Header:           {:>8.2} MB", Header::STORAGE_SIZE as f64 / 1e6);
    println!("Grammar:          {:>8.2} MB", grammar_sz as f64 / 1e6);
    println!("Trie (yada):      {:>8.2} MB", trie_sz as f64 / 1e6);
    println!("WordID+Params:    {:>8.2} MB", (rest_sz - wi_index_sz - wi_records_sz) as f64 / 1e6);
    println!("WordInfos index:  {:>8.2} MB  ({} words × 4)", wi_index_sz as f64 / 1e6, word_count);
    println!("WordInfos records:{:>8.2} MB  ← chunk-compress this", wi_records_sz as f64 / 1e6);
    println!("Total:            {:>8.2} MB", total as f64 / 1e6);

    if cli.sizes { return Ok(()); }

    let output = cli.output.unwrap_or_else(|| cli.dict.with_extension("xdic"));

    // --- xcdat trie + rank_to_offset table ---
    // For each sorted unique surface, record its byte offset in the word_id_table.
    // The yada trie stores byte offsets as values; we extract them via common_prefix_iterator.
    let lex_bytes = &data[lex_off..];
    let (yada_lex, _) = sudachi::dic::lexicon::Lexicon::parse(lex_bytes, 0, loader.header.has_synonym_group_ids())?;

    // Collect all unique surfaces with their word_id_table byte offsets
    let mut surface_to_offset: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for i in 0..loader.lexicon.size() {
        let surface = loader.lexicon.get_word_info(i, InfoSubset::SURFACE)?.surface().to_string();
        if surface_to_offset.contains_key(&surface) { continue; }
        let bytes = surface.as_bytes().to_vec();
        if let Some(entry) = yada_lex.lookup_raw(&bytes, 0) {
            surface_to_offset.insert(surface, entry);
        }
    }

    // rank_to_offset: sorted by surface (xcdat rank order) → byte offset
    let surfaces: Vec<String> = surface_to_offset.keys().cloned().collect();

    eprint!("{} surfaces → xcdat... ", surfaces.len());
    let tmp_keys = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut tmp_keys.as_file(), surfaces.join("\n").as_bytes())?;
    let tmp_xcdat = tempfile::NamedTempFile::new()?;
    anyhow::ensure!(
        std::process::Command::new(&cli.xcdat_build)
            .arg(tmp_keys.path()).arg(tmp_xcdat.path()).arg("-t").arg("8")
            .stdout(std::process::Stdio::null()).status()
            .map_err(|e| anyhow::anyhow!("xcdat_build not found (XCDAT_BUILD_BIN): {e}"))?.success(),
        "xcdat_build failed"
    );
    let xcdat_bytes = std::fs::read(tmp_xcdat.path())?;
    eprintln!("{:.2} MB", xcdat_bytes.len() as f64 / 1e6);

    // Enumerate xcdat to get rank → surface in xcdat's actual order, then map to word_id_table offsets.
    // xcdat's sort order (raw byte order via std::sort) may differ from Rust's BTreeMap<String> order
    // for certain byte sequences, so we must not assume they match.
    eprint!("Enumerating xcdat ranks... ");
    let enumerate_out = std::process::Command::new(&cli.xcdat_enumerate)
        .arg(tmp_xcdat.path())
        .output()
        .map_err(|e| anyhow::anyhow!("xcdat_enumerate not found (XCDAT_ENUMERATE_BIN): {e}"))?;
    anyhow::ensure!(enumerate_out.status.success(), "xcdat_enumerate failed");
    let mut rank_to_offset: Vec<u32> = Vec::with_capacity(surfaces.len());
    for line in std::str::from_utf8(&enumerate_out.stdout)?.lines() {
        let (rank_str, surface) = line.split_once('\t')
            .ok_or_else(|| anyhow::anyhow!("unexpected xcdat_enumerate output: {line}"))?;
        let rank: usize = rank_str.parse()?;
        let offset = *surface_to_offset.get(surface)
            .ok_or_else(|| anyhow::anyhow!("xcdat surface not in map: {surface}"))?;
        if rank >= rank_to_offset.len() { rank_to_offset.resize(rank + 1, 0); }
        rank_to_offset[rank] = offset;
    }
    eprintln!("{} ranks", rank_to_offset.len());

    // --- Train zstd dictionary on WordInfos record samples ---
    let wi_records = &data[wi_off + wi_index_sz..];
    eprint!("Training zstd dictionary on {} samples... ", DICT_TRAIN_SAMPLES);
    let step = wi_records.len() / DICT_TRAIN_SAMPLES;
    let samples: Vec<&[u8]> = (0..DICT_TRAIN_SAMPLES)
        .map(|i| {
            let start = (i * step).min(wi_records.len());
            let end = (start + 256).min(wi_records.len());
            &wi_records[start..end]
        })
        .collect();
    let zdict = zstd::dict::from_samples(&samples, ZDICT_SIZE)?;
    eprintln!("{} bytes", zdict.len());

    // --- Chunk-compress WordInfos records ---
    eprint!("Chunk-compressing WordInfos records ({} KB chunks)... ", cli.chunk_size / 1024);
    let mut chunk_offsets: Vec<u32> = Vec::new(); // byte offset of each compressed chunk
    let mut compressed_chunks: Vec<u8> = Vec::new();
    let mut encoder = zstd::bulk::Compressor::with_dictionary(cli.level, &zdict)?;
    for chunk in wi_records.chunks(cli.chunk_size) {
        chunk_offsets.push(compressed_chunks.len() as u32);
        let compressed = encoder.compress(chunk)?;
        compressed_chunks.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        compressed_chunks.extend_from_slice(&compressed);
    }
    eprintln!("{:.2} MB ({:.1}%)", compressed_chunks.len() as f64 / 1e6,
        compressed_chunks.len() as f64 / wi_records.len() as f64 * 100.0);

    // --- zstd grammar (single block, decompressed once at load) ---
    let grammar_raw = &data[Header::STORAGE_SIZE..lex_off];
    eprint!("zstd grammar... ");
    let grammar_zstd = zstd::bulk::Compressor::with_dictionary(cli.level, &zdict)?.compress(grammar_raw)?;
    eprintln!("{:.2} MB ({:.1}%)", grammar_zstd.len() as f64 / 1e6,
        grammar_zstd.len() as f64 / grammar_raw.len() as f64 * 100.0);

    // --- Write .xdic ---
    // Format:
    //   [header: STORAGE_SIZE bytes]
    //   [4: grammar_zstd_len][grammar_zstd]
    //   [4: xcdat_len][xcdat]
    //   [4: rank_to_offset_count][rank_to_offset — u32 per xcdat rank → word_id_table byte offset]
    //   [4: rest_len][word_id_table + word_params — uncompressed]
    //   [4: wi_index_len = word_count*4][wi_index — uncompressed]
    //   [4: zdict_len][zdict]
    //   [4: chunk_count][chunk_count * 4: chunk_offsets][compressed_chunks]
    let rest_uncompressed = &data[lex_off + trie_sz .. wi_off]; // word_id_table + word_params
    let wi_index = &data[wi_off .. wi_off + wi_index_sz];

    let mut out = std::fs::File::create(&output)?;
    out.write_all(&data[..Header::STORAGE_SIZE])?;
    out.write_all(&(grammar_zstd.len() as u32).to_le_bytes())?;
    out.write_all(&grammar_zstd)?;
    out.write_all(&(xcdat_bytes.len() as u32).to_le_bytes())?;
    out.write_all(&xcdat_bytes)?;
    out.write_all(&(rank_to_offset.len() as u32).to_le_bytes())?;
    for off in &rank_to_offset {
        out.write_all(&off.to_le_bytes())?;
    }
    out.write_all(&(rest_uncompressed.len() as u32).to_le_bytes())?;
    out.write_all(rest_uncompressed)?;
    out.write_all(&(wi_index_sz as u32).to_le_bytes())?;
    // Write wi_index with offsets relative to wi_records_start (subtract wi_off + wi_index_sz)
    let wi_records_start = wi_off + wi_index_sz;
    for i in 0..word_count {
        let abs_off = u32::from_le_bytes(wi_index[i*4..i*4+4].try_into().unwrap());
        let rel_off = abs_off - wi_records_start as u32;
        out.write_all(&rel_off.to_le_bytes())?;
    }
    out.write_all(&(zdict.len() as u32).to_le_bytes())?;
    out.write_all(&zdict)?;
    out.write_all(&(chunk_offsets.len() as u32).to_le_bytes())?;
    for co in &chunk_offsets {
        out.write_all(&co.to_le_bytes())?;
    }
    out.write_all(&compressed_chunks)?;

    let new_size = std::fs::metadata(&output)?.len() as usize;
    println!("\nOriginal:    {:>8.2} MB", total as f64 / 1e6);
    println!("Compressed:  {:>8.2} MB  ({:.1}% of original)", new_size as f64 / 1e6, new_size as f64 / total as f64 * 100.0);
    println!("RAM per lookup: ~{} KB (one chunk decompressed)", cli.chunk_size / 1024);
    println!("Saved to {}", output.display());
    Ok(())
}
