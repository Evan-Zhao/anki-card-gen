use csv;
use regex::Regex;
use reqwest;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::error;
use std::fs::File;
use std::iter::zip;
use std::vec::Vec;
use tl;
use tl::{HTMLTag, Node, Parser};

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
    let mut hits = tag.query_selector(parser, query)?;
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

fn split_at_h3_h4<'a, 'b>(nodes: &'b [&'a Node<'a>]) -> Vec<&'b [&'a Node<'a>]> {
    let splitters: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|pair| node_tag_predicate(pair.1, |t| t.name() == "h3" || t.name() == "h4"))
        .map(|pair| pair.0)
        .collect();
    zip(splitters[..].iter(), splitters[1..].iter())
        .map(|pair| &nodes[*pair.0..*pair.1])
        .collect()
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

type ExamplePair = (String, Option<String>);

fn parse_meaning_item(parser: &Parser, node: &Node) -> Option<(String, Vec<ExamplePair>)> {
    fn parse_example(parser: &Parser, node: &Node, inline_mode: bool) -> Option<ExamplePair> {
        let orig_query = if inline_mode {
            "i.Latn.mention.e-example"
        } else {
            "span.e-quotation"
        };
        let orig = query_select_first(parser, node, orig_query)?.as_tag()?;
        let orig = orig.inner_text(parser).into_owned();
        let trans = query_select_first(parser, node, "span.e-translation");
        let trans = match trans {
            Some(Node::Tag(tag)) => Some(tag.inner_text(parser).into_owned()),
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
    let meaning = meanings.split("\n").next()?.trim().to_string();
    Some((meaning, examples))
}

fn shorten_meaning(meaning: String) -> String {
    let re = Regex::new(r"^(.*) \((.*)\)$").unwrap();
    let maybe_match = re.captures_iter(meaning.as_str()).next();
    match maybe_match {
        Some(match_) if match_[2].len() >= 15 => match_[1].to_owned(),
        _ => meaning,
    }
}

fn get_meaning_from_section<'a>(
    parser: &Parser<'a>,
    section: &[&'a Node<'a>],
) -> Option<(String, Option<ExamplePair>)> {
    let ol = find_first_node_by_name(&section[1..], "ol")?;
    let children = get_children(parser, ol)?;
    let mut meanings: Vec<String> = Vec::new();
    let mut examples: Vec<ExamplePair> = Vec::new();
    for node in children {
        match node.as_tag() {
            Some(tag) if tag.name() == "li" => {
                let (meaning, examples_) = parse_meaning_item(parser, node)?;
                meanings.push(shorten_meaning(meaning).to_owned());
                examples.extend(examples_)
            }
            _ => {
                continue;
            }
        }
    }
    Some((meanings.join("; "), examples.iter().next().cloned()))
}

fn example_remove_trans(example: ExamplePair) -> (String, String) {
    let (sentence, transl) = example;
    let transl = transl.unwrap_or("".to_string());
    (format!("{sentence} -- {transl}"), sentence)
}

async fn wiktionary_lookup(
    word: &str,
) -> Option<HashMap<&str, Option<String>>> {
    let url = format!("https://en.wiktionary.org/wiki/{word}");
    let res = reqwest::get(url.as_str()).await.ok()?;
    let body = res.text().await.ok()?;
    let dom = tl::parse(body.borrow(), tl::ParserOptions::default()).ok()?;
    let parser = dom.parser();
    let mw_parser_output = dom.query_selector("div.mw-parser-output").unwrap().next()?.get(parser).unwrap();
    let body_elems = get_children(parser, mw_parser_output)?;
    let french_tags = split_and_take(parser, &body_elems, "h2", "French")?;
    let sections = split_at_h3_h4(french_tags);
    let mut ipa: Option<String> = None;
    let mut meaning: Option<String> = None;
    let mut example: Option<ExamplePair> = None;
    for section in sections {
        let header_text = get_header_text(parser, section[0]);
        if header_text.is_none() {
            continue;
        }
        match header_text.unwrap().as_str() {
            "Pronunciation" => {
                ipa = find_first_node_by_name(&section[1..], "ul")
                    .and_then(|ul| query_select_first(parser, ul, "span.IPA"))
                    .and_then(|ipa_node| get_immediate_text(parser, ipa_node));
            }
            "Noun" | "Pronoun" | "Verb" | "Adjective" | "Adverb" | "Numeral" | "Determiner"
            | "Interjection" => {
                let (meaning_, example_) = get_meaning_from_section(parser, section)?;
                meaning = Some(meaning_);
                example = example_;
            }
            _ => (),
        }
    }
    let (ex_trans, ex_untrans) = match example {
        Some(example_) => {
            let (lhs, rhs) = example_remove_trans(example_);
            (Some(lhs), Some(rhs))
        }
        None => (None, None),
    };
    Some(HashMap::from([
        ("word", Some(word.to_owned())),
        ("word_with_article", None),
        ("frequency_index", None),
        ("IPA", ipa),
        ("noun_declention", None),
        ("meaning", meaning),
        ("example", ex_trans),
        ("example_untranslated", ex_untrans),
        ("wiki_link", Some(format!("{url}#French"))),
        ("verb_declention", None),
        ("audio_parisian", Some(format!("[sound:]"))),
        ("audio_quebecois", None),
    ]))
}

fn read_words_from_csv(filename: &str) -> Result<Vec<String>, Box<dyn error::Error>> {
    let file = File::open(filename)?;
    let mut words: Vec<String> = Vec::new();
    for result in csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_reader(file)
        .records()
    {
        let record = result?;
        let record_vec: Vec<&str> = record.iter().filter(|s| !s.is_empty()).collect();
        match record_vec[..] {
            [word, _] => words.push(word.to_owned()),
            _ => Err("Incorrect number of fields in record")?,
        }
    }
    Ok(words)
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for word in read_words_from_csv("data.txt").unwrap() {
        let res = rt.block_on(wiktionary_lookup(word.as_str()));
        println!("{} => {:?}\n\n", word, res);
    }
}
