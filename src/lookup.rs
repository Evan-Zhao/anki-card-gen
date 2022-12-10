use regex::Regex;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error;
use std::iter::zip;
use std::vec::Vec;
use tl;
use tl::{HTMLTag, Node, Parser};

pub type ResultOrError<T> = Result<T, Box<dyn error::Error>>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum NounGender {
    Masculine,
    Feminine,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PartOfSpeech {
    Noun {
        gender: Option<NounGender>,
    },
    Verb,
    Adjective {
        f: Option<String>,
        mp: Option<String>,
        fp: Option<String>,
    },
    Pronoun,
    Adverb,
    Numeral,
    Determiner,
    Preposition,
    Interjection,
}

pub type Example = (String, Option<String>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meaning {
    pub pos: PartOfSpeech,
    pub meaning: String,
    pub examples: Vec<Example>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Pronunciation {
    pub ipa: String,
    pub audio_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Word {
    pub word: String,
    pub wiki_link: String,
    pub pronunciation: Option<Pronunciation>,
    pub meanings: Vec<Meaning>,
}

fn get_children<'a>(parser: &'a Parser<'a>, node: &'a Node<'a>) -> Option<Vec<&'a Node<'a>>> {
    let handles = node.children()?;
    let children_iter = handles.top().iter();
    Some(
        children_iter
            .map(|handle| handle.get(parser).unwrap())
            .collect(),
    )
}

fn query_select_first<'a>(
    parser: &'a Parser<'a>,
    node: &'a Node<'a>,
    query: &str,
) -> Option<&'a Node<'a>> {
    let tag = node.as_tag()?;
    let mut hits = tag
        .query_selector(parser, query)
        .expect("Failed to parse query");
    let first_hit = hits.next()?;
    first_hit.get(parser)
}

fn get_immediate_text(parser: &Parser, node: &Node) -> Option<String> {
    let children = get_children(parser, node)?;
    match children[..] {
        [Node::Raw(data)] => Some(data.as_utf8_str().into_owned()),
        _ => None,
    }
}

fn get_header_text(parser: &Parser, node: &Node) -> Option<String> {
    let first_hit = query_select_first(parser, node, "span.mw-headline")?;
    get_immediate_text(parser, first_hit)
}

fn node_tag_predicate<Pred>(node: &Node, predicate: Pred) -> bool
where
    Pred: Fn(&HTMLTag) -> bool,
{
    node.as_tag().map(predicate).unwrap_or(false)
}

fn split_and_take<'a, 'b>(
    parser: &Parser<'a>,
    nodes: &'b Vec<&'a Node<'a>>,
    header_level: &str,
    header_content: &str,
) -> Option<&'b [&'a Node<'a>]> {
    let content_match = |node: &Node| {
        get_header_text(parser, node)
            .map(|text| text == header_content)
            .unwrap_or(false)
    };
    let splitters: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|pair| node_tag_predicate(pair.1, |t| t.name() == header_level))
        .map(|pair| pair.0)
        .collect();
    let match_ = splitters
        .iter()
        .position(|idx| content_match(&nodes[*idx]))?;
    if match_ < splitters.len() - 1 {
        Some(&nodes[splitters[match_]..splitters[match_ + 1]])
    } else {
        Some(&nodes[splitters[match_]..])
    }
}

fn split_at_h3_h4<'a, 'b>(nodes: &'b [&'a Node<'a>]) -> Vec<Vec<&'a Node<'a>>> {
    let splitters: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|pair| node_tag_predicate(pair.1, |t| t.name() == "h3" || t.name() == "h4"))
        .map(|pair| pair.0)
        .collect();
    zip(splitters[..].iter(), splitters[1..].iter())
        .map(|pair| nodes[*pair.0..*pair.1].iter().map(|&x| x).collect())
        .collect()
}

fn fetch_french_sections<'a>(
    dom: &'a tl::VDom,
    parser: &'a Parser,
) -> Option<Vec<Vec<&'a Node<'a>>>> {
    let mw_parser_output = dom
        .query_selector("div.mw-parser-output")
        .unwrap()
        .next()?
        .get(parser)
        .unwrap();
    let body_elems = get_children(parser, mw_parser_output)?;
    let french_tags = split_and_take(parser, &body_elems, "h2", "French")?;
    Some(split_at_h3_h4(french_tags))
}

fn find_first_node_by_name<'a>(nodes: &[&'a Node<'a>], node_name: &str) -> Option<&'a Node<'a>> {
    for node in nodes {
        let tag = node.as_tag();
        if tag.is_none() {
            continue;
        }
        if tag.unwrap().name() == node_name {
            return Some(node);
        }
    }
    return None;
}

fn parse_meaning_item(parser: &Parser, node: &Node, pos: PartOfSpeech) -> Option<Meaning> {
    fn parse_example(parser: &Parser, node: &Node, inline_mode: bool) -> Option<Example> {
        let orig_query = if inline_mode {
            "i.Latn.mention.e-example"
        } else {
            "span.e-quotation"
        };
        let orig = query_select_first(parser, node, orig_query)?.as_tag()?;
        let orig = orig.inner_html(parser);
        let trans = query_select_first(parser, node, "span.e-translation");
        let trans = match trans {
            Some(Node::Tag(tag)) => Some(tag.inner_html(parser)),
            Some(_) => return None,
            None => None,
        };
        Some((orig, trans))
    }

    // Collect all tags before the first .dl as the meaning,
    // and parse the last .dl tag into examples.
    let children = get_children(parser, node)?;
    let last_node = children.iter().rev().next()?;
    let examples = match last_node.as_tag() {
        // Inline style
        Some(tag) if tag.name() == "dl" => tag
            .query_selector(parser, "div.h-usage-example")?
            .filter_map(|handle| parse_example(parser, handle.get(parser)?, true))
            .collect(),
        // Expandable quotation style
        Some(tag) if tag.name() == "ul" => tag
            .query_selector(parser, "li")?
            .filter_map(|handle| parse_example(parser, handle.get(parser)?, false))
            .collect(),
        _ => vec![],
    };
    let meanings = children
        .iter()
        .map(|node| node.inner_text(parser).into_owned())
        .collect::<Vec<String>>()
        .join("");
    // Meaning text only lasts 1 line.
    let meaning = shorten_meaning(meanings.split("\n").next()?.trim().to_string());
    Some(Meaning {
        pos,
        meaning,
        examples,
    })
}

fn shorten_meaning(meaning: String) -> String {
    let re = Regex::new(r"^(.*) \((.*)\)$").unwrap();
    let maybe_match = re.captures_iter(meaning.as_str()).next();
    match maybe_match {
        Some(match_) if match_[2].len() >= 15 => match_[1].to_owned(),
        _ => meaning,
    }
}

fn get_meanings_from_section<'a>(
    parser: &Parser<'a>,
    section: Vec<&'a Node<'a>>,
    pos: PartOfSpeech,
) -> ResultOrError<Vec<Meaning>> {
    let ol = find_first_node_by_name(&section[1..], "ol").ok_or("Cannot find <ol>")?;
    let children = get_children(parser, ol).expect("");
    let ret = children
        .iter()
        .filter_map(|node| {
            let tag_ = node.as_tag();
            if tag_.is_none() || tag_.unwrap().name() != "li" {
                return None;
            }
            parse_meaning_item(parser, node, pos.clone())
        })
        .collect::<Vec<Meaning>>();
    Ok(ret)
}

fn get_gender_from_section(parser: &Parser, section: &Vec<&Node>) -> Option<NounGender> {
    // tl query selector doesn't seem to work on composite selectors (like `span abbr`)
    let outer_node = find_first_node_by_name(section, "p")?;
    let abbr_node = query_select_first(parser, outer_node, "abbr")?;
    match get_immediate_text(parser, abbr_node) {
        Some(s) if s == "m" => Some(NounGender::Masculine),
        Some(s) if s == "f" => Some(NounGender::Feminine),
        _ => None,
    }
}

fn get_pr_from_section(parser: &Parser, section: &Vec<&Node>) -> Option<Pronunciation> {
    let subsec = find_first_node_by_name(&section[1..], "ul")?;
    let ipa_node = query_select_first(parser, subsec, "span.IPA")?;
    let ipa = get_immediate_text(parser, ipa_node)?;
    let audio_tag = query_select_first(parser, subsec, "source")?.as_tag()?;
    let audio_href = audio_tag.attributes().get("src")??.as_utf8_str();
    let audio_url = format!("https:{}", audio_href);
    Some(Pronunciation { ipa, audio_url })
}

fn get_adj_form_from_section(parser: &Parser, section: &Vec<&Node>) -> Option<PartOfSpeech> {
    let outer_node = find_first_node_by_name(section, "p")?;
    let children: Vec<&HTMLTag> = get_children(parser, outer_node)?
        .iter()
        .filter_map(|node| node.as_tag())
        .collect();
    let mut f: Option<String> = None;
    let mut mp: Option<String> = None;
    let mut fp: Option<String> = None;
    for idx in 0..children.len() - 1 {
        let this_tag = children[idx];
        let next_tag = children[idx + 1];
        if this_tag.name() != "i" || next_tag.name() != "b" {
            continue;
        }
        let label = this_tag.inner_text(parser).to_string();
        let form = next_tag.inner_text(parser).to_string();
        match label.as_str() {
            "feminine" => f = Some(form),
            "masculine plural" => mp = Some(form),
            "feminine plural" => fp = Some(form),
            _ => continue,
        };
    }
    Some(PartOfSpeech::Adjective { f, mp, fp })
}

pub async fn request_w_header(url: &str) -> ResultOrError<reqwest::Response> {
    let client = reqwest::Client::new();
    Ok(client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            "AnkiBot/0.1 (connorzh3@gmail.com)",
        )
        .send()
        .await?)
}

pub async fn wiktionary_lookup(word: &str) -> ResultOrError<Word> {
    let part_of_speech_name: HashMap<&str, PartOfSpeech> = HashMap::from([
        ("Verb", PartOfSpeech::Verb),
        ("Pronoun", PartOfSpeech::Pronoun),
        ("Adverb", PartOfSpeech::Adverb),
        ("Numeral", PartOfSpeech::Numeral),
        ("Determiner", PartOfSpeech::Determiner),
        ("Preposition", PartOfSpeech::Preposition),
        ("Interjection", PartOfSpeech::Interjection),
    ]);

    let url = format!("https://en.wiktionary.org/wiki/{word}");
    let res = request_w_header(url.as_str()).await?;
    let body = res.text().await?;
    let dom = tl::parse(body.as_str(), tl::ParserOptions::default())?;
    let parser = dom.parser();
    let sections = fetch_french_sections(&dom, parser).ok_or(format!("Cannot find word {word}"))?;
    let mut pronunciation: Option<Pronunciation> = None;
    let mut meanings = Vec::<Meaning>::new();
    for section in sections {
        let header_text = get_header_text(parser, section[0]);
        if header_text.is_none() {
            continue;
        }
        match header_text.unwrap().as_str() {
            "Pronunciation" => {
                pronunciation = get_pr_from_section(parser, &section);
            }
            "Noun" => {
                let gender = get_gender_from_section(parser, &section);
                let meanings_ =
                    get_meanings_from_section(parser, section, PartOfSpeech::Noun { gender })?;
                meanings.extend(meanings_);
            }
            "Adjective" => {
                let adj_forms = get_adj_form_from_section(parser, &section)
                    .ok_or("Adjective forms parsing failed")?;
                let meanings_ = get_meanings_from_section(parser, section, adj_forms)?;
                meanings.extend(meanings_);
            }
            s if part_of_speech_name.contains_key(s) => {
                let pos = part_of_speech_name[s].clone();
                let meanings_ = get_meanings_from_section(parser, section, pos)?;
                meanings.extend(meanings_);
            }
            _ => (),
        }
    }
    Ok(Word {
        word: word.to_owned(),
        wiki_link: format!("{url}#French"),
        pronunciation,
        meanings,
    })
}
