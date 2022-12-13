use glob::glob;
use serde_json as json;
use std::collections::HashMap;
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

fn read_words_from(glob_pattern: &str) -> ResultOrError<HashMap<String, HashMap<String, String>>> {
    let mut words: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut n_words: isize = 0;
    let line_pattern = Regex::new(r"^([^\[]+) \[(.*)+\]$").unwrap();
    for filename in glob(glob_pattern).expect("Failed to parse glob pattern") {
        let filename = filename?;
        let file_content = fs::read_to_string(&filename)?;
        let stem = filename.file_stem().unwrap().to_str().unwrap().to_string();
        let lines = file_content.split("\n").filter(|s| !s.is_empty());
        let mut inner: HashMap<String, String> = HashMap::new();
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
                    inner.insert(word, meaning);
                    n_words += 1;
                }
                None => {
                    println!("Line '{}' is malformed", line);
                    continue;
                }
            }
        }
        words.insert(stem, inner);
    }
    println!("Found {} words", n_words);
    Ok(words)
}

async fn word_to_anki_fields(
    record: Word,
    select_meaning: &str,
    audio_dir: &str,
) -> ResultOrError<Vec<String>> {
    fn format_example(example: Example) -> (String, String) {
        let (sentence, transl) = example;
        (sentence, transl.unwrap_or("".to_string()))
    }

    fn format_genders(word: &str, meaning: &Meaning) -> String {
        let first_ch = word.chars().nth(0).expect("Word is empty");
        let is_vowel = match first_ch {
            'a' | 'e' | 'i' | 'o' | 'u' | 'h' => true,
            _ => false,
        };
        let is_masc = match meaning.pos {
            PartOfSpeech::Noun {
                gender: Some(NounGender::Masculine),
            } => true,
            PartOfSpeech::Noun {
                gender: Some(NounGender::Feminine),
            } => false,
            _ => {
                return "".to_string();
            }
        };
        match (is_vowel, is_masc) {
            (true, true) => format!("l'{word} (masc.)"),
            (true, false) => format!("l'{word} (fem.)"),
            (false, true) => format!("le {word}"),
            (false, false) => format!("la {word}"),
        }
    }

    fn match_meaning<'a>(
        word: &str,
        meanings: &'a Vec<Meaning>,
        select_meaning: &str,
    ) -> ResultOrError<&'a Meaning> {
        let mut meaning = None;
        let mut is_ambiguous = false;
        let all_meanings_str = meanings
            .iter()
            .map(|m| "  ".to_string() + &m.meaning)
            .collect::<Vec<_>>()
            .join("\n");
        for meaning_ in meanings {
            let is_match = meaning_.meaning.contains(select_meaning);
            if is_match {
                if meaning.is_some() {
                    is_ambiguous = true;
                }
                meaning = Some(meaning_);
            }
        }
        if is_ambiguous {
            println!("Ambiguous meaning '{select_meaning}' for word '{word}'; choose from \n{all_meanings_str}\n");
        }
        match meaning {
            Some(meaning) => Ok(meaning),
            None => {
                println!(
                    "No meaning of '{word}' matches the given meaning '{select_meaning}'. Select from:\n{all_meanings_str}");
                Err(format!("No matching meaning for '{word}'"))?
            }
        }
    }

    let word = record.word;
    let meanings = record.meanings;
    if meanings.len() == 0 {
        Err(format!("Word '{word}' without meaning is malformed"))?
    }
    let meaning = match_meaning(&word, &meanings, select_meaning)?;
    let word_w_article = format_genders(&word, &meaning);
    let examples = &meaning.examples;
    let (ex_w_trans, ex_wo_trans) = if examples.len() > 0 {
        format_example(examples[0].clone())
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
        select_meaning.to_string(),
        ex_w_trans,
        ex_wo_trans,
        record.wiki_link,
        "".to_string(),
        audio_file_entry,
    ])
}

async fn look_up_all(glob_pattern: &str, audio_dir: &str, json_f: &str) -> ResultOrError<()> {
    let to_look_up = read_words_from(glob_pattern)?;
    let mut words = Vec::<Word>::new();
    for (filename, to_look_up_) in to_look_up {
        let mut out_f = fs::File::create(format!("{filename}.txt"))?;
        for (word_str, meaning) in to_look_up_ {
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
    }
    fs::write(json_f, json::to_string(&words)?)?;
    Ok(())
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = look_up_all("./words/*.txt", "audio/", "words.json");
    rt.block_on(result).unwrap();
}
