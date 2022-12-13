use glob::glob;
use serde_json as json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path;
use std::vec::Vec;
mod lookup;
use lookup::{
    request_w_header, wiktionary_lookup, Example, Meaning, NounGender, PartOfSpeech, ResultOrError,
    Word,
};
use regex::Regex;

fn read_words_from(glob_pattern: &str) -> ResultOrError<HashMap<String, String>> {
    let mut words: HashMap<String, String> = HashMap::new();
    let line_pattern = Regex::new(r"^([^[]+) [(.*)+]$").unwrap();
    for entry in glob(glob_pattern).expect("Failed to parse glob pattern") {
        let file_content = fs::read_to_string(entry?)?;
        let lines = file_content.split("\n").filter(|s| !s.is_empty());
        for line in lines {
            let maybe_match = line_pattern.captures_iter(line).next();
            match maybe_match {
                Some(match_) => {
                    let word = match_[1].to_string();
                    if words.contains_key(&word) {
                        println!("Duplicate word '{}'", word);
                        continue;
                    }
                    let meaning = match_[2].to_string();
                    words.insert(word, meaning);
                }
                None => {
                    println!("Line '{}' is malformed", line);
                    continue;
                }
            }
        }
    }
    Ok(words)
}

async fn word_to_anki_fields(record: Word, select_meaning: &str, audio_dir: &str) -> ResultOrError<Vec<String>> {
    fn shorten_meaning(meaning: &str) -> String {
        let re = Regex::new(r"^(.*) \((.*)\)$").unwrap();
        let maybe_match = re.captures_iter(meaning).next();
        match &maybe_match {
            Some(match_) if match_[2].len() >= 15 => match_[1].to_string(),
            _ => meaning.to_string(),
        }
    }

    fn fuse_shorten_meaning<'a>(meanings: &Vec<Meaning>) -> String {
        meanings
            .iter()
            .map(|m| shorten_meaning(m.meaning.as_str()))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn format_example(example: Example) -> (String, String) {
        let (sentence, transl) = example;
        let transl = transl.unwrap_or("".to_string());
        (format!("{sentence} -- {transl}"), sentence)
    }

    fn format_genders<'a>(word: &str, meanings: impl Iterator<Item = &'a Meaning>) -> String {
        let first_ch = word.chars().nth(0).expect("Word is empty");
        let is_vowel = match first_ch {
            'a' | 'e' | 'i' | 'o' | 'u' | 'h' => true,
            _ => false,
        };
        let genders = meanings
            .filter_map(|meaning| match &meaning.pos {
                PartOfSpeech::Noun { gender } => *gender,
                _ => None,
            })
            .collect::<HashSet<_>>();
        let is_m = genders.contains(&NounGender::Masculine);
        let is_f = genders.contains(&NounGender::Feminine);
        if (is_m && is_f) || (!is_m && !is_f) {
            // TODO: if some meanings are masc. and some are fem.,
            // display gender at each meaning instead
            return "".to_string();
        }
        match (is_vowel, is_m) {
            (true, true) => format!("l'{word} (masc.)"),
            (true, false) => format!("l'{word} (fem.)"),
            (false, true) => format!("le {word}"),
            (false, false) => format!("la {word}"),
        }
    }

    let word = record.word;
    if record.meanings.len() == 0 {
        Err(format!("Word '{word}' without meaning is malformed"))?
    }
    let mut meanings = record.meanings;
    let meaning_str = fuse_shorten_meaning(&meanings);
    let word_w_article = format_genders(&word, meanings.iter());
    // let examples: Vec<_> = meanings.iter().filter_map(|meaning| meaning.examples.iter().next()).collect();
    let mut examples = meanings.remove(0).examples;
    let (ex_w_trans, ex_wo_trans) = if examples.len() > 0 {
        format_example(examples.remove(0))
    } else {
        ("".to_string(), "".to_string())
    };
    let (ipa, audio_file_entry) = match record.pronunciation {
        Some(pronunciation) => {
            let audio_url = pronunciation.audio_url;
            let path_str = format!("{audio_dir}/{word}.mp3");
            let path = path::Path::new(&path_str);
            if !path.exists() {
                let audio = request_w_header(&audio_url).await?.bytes().await?;
                let mut audio_f = fs::File::create(path)?;
                audio_f.write(&audio)?;
            }
            (pronunciation.ipa, format!("[sound:{word}.mp3]"))
        }
        None => ("".to_string(), "".to_string()),
    };
    Ok(vec![
        word,
        word_w_article,
        "".to_string(),
        ipa,
        "".to_string(),
        meaning_str,
        ex_w_trans,
        ex_wo_trans,
        record.wiki_link,
        "".to_string(),
        audio_file_entry,
    ])
}

async fn look_up_all(
    glob_pattern: &str,
    anki_f: &str,
    audio_dir: &str,
    json_f: &str,
) -> ResultOrError<()> {
    let to_look_up = read_words_from(glob_pattern)?;
    println!("Found {} words", to_look_up.len());
    let mut words = Vec::<Word>::new();
    let mut out_f = fs::File::create(anki_f)?;
    for (word_str, meaning) in to_look_up {
        match wiktionary_lookup(&word_str).await {
            Ok(word) => {
                words.push(word.clone());
                for field in word_to_anki_fields(word, &meaning, audio_dir).await? {
                    out_f.write(field.as_bytes())?;
                    out_f.write("\t".as_bytes())?;
                }
                out_f.write("\n".as_bytes())?;
            }
            Err(err) => {
                println!("Failed to look up '{}' due to error '{}'", word_str, err);
                continue;
            }
        }
    }
    fs::write(json_f, json::to_string(&words)?)?;
    Ok(())
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = look_up_all("./words/*.txt", "anki.txt", "audio/", "words.json");
    rt.block_on(result).unwrap();
}
