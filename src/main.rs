use glob::glob;
use serde_json as json;
use std::fs;
use std::io::Write;
use std::vec::Vec;
use std::{collections::HashSet, path};
mod lookup;
use lookup::{
    request_w_header, wiktionary_lookup, Example, Meaning, NounGender, PartOfSpeech, ResultOrError,
    Word,
};
use regex::Regex;

async fn look_up_all(glob_pattern: &str) -> ResultOrError<Vec<Word>> {
    let mut words: HashSet<String> = HashSet::new();
    for entry in glob(glob_pattern).expect("Failed to parse glob pattern") {
        let file_content = fs::read_to_string(entry?)?;
        let words_ = file_content.split("\n").filter(|s| !s.is_empty());
        for word in words_ {
            if words.contains(word) {
                println!("Duplicate word '{}'", word);
                continue;
            }
            words.insert(word.to_owned());
        }
    }
    println!("Found {} words", words.len());
    let mut data = Vec::<Word>::new();
    for word in words {
        match wiktionary_lookup(word.as_str()).await {
            Ok(json) => {
                data.push(json);
            }
            Err(err) => {
                println!("Failed to look up '{}' due to error '{}'", word, err);
                continue;
            }
        }
    }
    Ok(data)
}

async fn word_to_anki_fields(record: Word, audio_dir: &str) -> ResultOrError<Vec<String>> {
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
            return "".to_string()
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
    let ipa = match &record.pronunciation {
        Some(pronunciation) => pronunciation.ipa.clone(),
        None => "".to_string(),
    };
    let audio_url = record.pronunciation.map(|pron| pron.audio_url);
    let mut audio_file_entry: String = "".to_string();
    if let Some(url) = &audio_url {
        let path_str = format!("{audio_dir}/{word}.mp3");
        let path = path::Path::new(&path_str);
        if !path.exists() {
            let audio = request_w_header(url.as_str()).await?.bytes().await?;
            let mut audio_f = fs::File::create(path)?;
            audio_f.write(&audio)?;
        }
        audio_file_entry = format!("[sound:{word}.mp3]");
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

async fn process_json_into(from: &str, to: &str, audio_dir: &str) -> ResultOrError<()> {
    let data = fs::read_to_string(from)?;
    let words: Vec<Word> = json::from_str(data.as_str())?;
    let mut out_f = fs::File::create(to)?;
    for word in words {
        for field in word_to_anki_fields(word, audio_dir).await? {
            out_f.write(field.as_bytes())?;
            out_f.write("\t".as_bytes())?;
        }
        out_f.write("\n".as_bytes())?;
    }
    Ok(())
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // let data = rt.block_on(look_up_all("./words/*.txt")).unwrap();
    // let serialized = json::to_string(&data).unwrap();
    // fs::write("collected.json", serialized).unwrap();
    rt.block_on(process_json_into("filtered.json", "anki.txt", "audio/"))
        .unwrap();
}
