use glob::glob;
use serde_json as json;
use std::fs;
use std::io::Write;
use std::vec::Vec;
use std::collections::HashSet;
mod lookup;
use lookup::{
    request_w_header, wiktionary_lookup, Example, NounGender, PartOfSpeech, ResultOrError, Word,
};

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
    fn format_gender(word: &String, gender: &NounGender) -> String {
        let first_ch = word.chars().nth(0).expect("Word is empty");
        match first_ch {
            'a' | 'e' | 'i' | 'o' | 'u' | 'h' => format!("l'{word}"),
            _ => match gender {
                NounGender::Masculine => format!("le {word}"),
                NounGender::Feminine => format!("la {word}"),
            },
        }
    }

    fn format_example(example: Example) -> (String, String) {
        let (sentence, transl) = example;
        let transl = transl.unwrap_or("".to_string());
        (format!("{sentence} -- {transl}"), sentence)
    }

    let word = record.word;
    if record.meanings.len() != 1 {
        Err(format!("Word '{word}' does not have exactly 1 meaning"))?
    }
    let mut meanings = record.meanings;
    let meaning = meanings.remove(0);
    let word_w_article = match &meaning.pos {
        PartOfSpeech::Noun {
            gender: Some(gender),
        } => format_gender(&word, &gender),
        PartOfSpeech::Noun { gender: None } => "".to_string(),
        _ => "".to_string(),
    };
    let ipa = match &record.pronunciation {
        Some(pronunciation) => pronunciation.ipa.clone(),
        None => "".to_string(),
    };
    let mut examples = meaning.examples;
    let (ex_w_trans, ex_wo_trans) = if examples.len() > 0 {
        format_example(examples.remove(0))
    } else {
        ("".to_string(), "".to_string())
    };
    let audio_url = record.pronunciation.map(|pron| pron.audio_url);
    let mut audio_file_entry: String = "".to_string();
    if let Some(url) = &audio_url {
        let audio = request_w_header(url.as_str()).await?.bytes().await?;
        let mut audio_f = fs::File::create(format!("{audio_dir}/{word}.mp3"))?;
        audio_f.write(&audio)?;
        audio_file_entry = format!("[sound:{word}.mp3]");
    };
    Ok(vec![
        word,
        word_w_article,
        "".to_string(),
        ipa,
        "".to_string(),
        meaning.meaning,
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
    let data = rt.block_on(look_up_all("./words/*.txt")).unwrap();
    let serialized = json::to_string(&data).unwrap();
    fs::write("collected.json", serialized).unwrap();
    // rt.block_on(process_json_into("collected.json", "anki.txt", "audio/"))
    //     .unwrap();
}
