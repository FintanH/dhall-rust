use itertools::Itertools;
use pest::iterators::Pair;
use pest::prec_climber as pcl;
use pest::prec_climber::PrecClimber;
use pest::Parser;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use dhall_generated_parser::{DhallParser, Rule};

use crate::map::{DupTreeMap, DupTreeSet};
use crate::ExprF::*;
use crate::*;

// This file consumes the parse tree generated by pest and turns it into
// our own AST. All those custom macros should eventually moved into
// their own crate because they are quite general and useful. For now they
// are here and hopefully you can figure out how they work.

pub(crate) type ParsedRawExpr = RawExpr<!>;
pub(crate) type ParsedExpr = Expr<!>;
type ParsedText = InterpolatedText<ParsedExpr>;
type ParsedTextContents = InterpolatedTextContents<ParsedExpr>;

pub type ParseError = pest::error::Error<Rule>;

pub type ParseResult<T> = Result<T, ParseError>;

#[derive(Debug)]
enum Either<A, B> {
    Left(A),
    Right(B),
}

impl crate::Builtin {
    pub fn parse(s: &str) -> Option<Self> {
        use crate::Builtin::*;
        match s {
            "Bool" => Some(Bool),
            "Natural" => Some(Natural),
            "Integer" => Some(Integer),
            "Double" => Some(Double),
            "Text" => Some(Text),
            "List" => Some(List),
            "Optional" => Some(Optional),
            "None" => Some(OptionalNone),
            "Natural/build" => Some(NaturalBuild),
            "Natural/fold" => Some(NaturalFold),
            "Natural/isZero" => Some(NaturalIsZero),
            "Natural/even" => Some(NaturalEven),
            "Natural/odd" => Some(NaturalOdd),
            "Natural/toInteger" => Some(NaturalToInteger),
            "Natural/show" => Some(NaturalShow),
            "Natural/subtract" => Some(NaturalSubtract),
            "Integer/toDouble" => Some(IntegerToDouble),
            "Integer/show" => Some(IntegerShow),
            "Double/show" => Some(DoubleShow),
            "List/build" => Some(ListBuild),
            "List/fold" => Some(ListFold),
            "List/length" => Some(ListLength),
            "List/head" => Some(ListHead),
            "List/last" => Some(ListLast),
            "List/indexed" => Some(ListIndexed),
            "List/reverse" => Some(ListReverse),
            "Optional/fold" => Some(OptionalFold),
            "Optional/build" => Some(OptionalBuild),
            "Text/show" => Some(TextShow),
            _ => None,
        }
    }
}

pub fn custom_parse_error(pair: &Pair<Rule>, msg: String) -> ParseError {
    let msg =
        format!("{} while matching on:\n{}", msg, debug_pair(pair.clone()));
    let e = pest::error::ErrorVariant::CustomError { message: msg };
    pest::error::Error::new_from_span(e, pair.as_span())
}

fn debug_pair(pair: Pair<Rule>) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    fn aux(s: &mut String, indent: usize, prefix: String, pair: Pair<Rule>) {
        let indent_str = "| ".repeat(indent);
        let rule = pair.as_rule();
        let contents = pair.as_str();
        let mut inner = pair.into_inner();
        let mut first = true;
        while let Some(p) = inner.next() {
            if first {
                first = false;
                let last = inner.peek().is_none();
                if last && p.as_str() == contents {
                    let prefix = format!("{}{:?} > ", prefix, rule);
                    aux(s, indent, prefix, p);
                    continue;
                } else {
                    writeln!(
                        s,
                        r#"{}{}{:?}: "{}""#,
                        indent_str, prefix, rule, contents
                    )
                    .unwrap();
                }
            }
            aux(s, indent + 1, "".into(), p);
        }
        if first {
            writeln!(
                s,
                r#"{}{}{:?}: "{}""#,
                indent_str, prefix, rule, contents
            )
            .unwrap();
        }
    }
    aux(&mut s, 0, "".into(), pair);
    s
}

macro_rules! parse_children {
    // Variable length pattern with a common unary variant
    (@match_forwards,
        $parse_args:expr,
        $iter:expr,
        ($body:expr),
        $variant:ident ($x:ident)..,
        $($rest:tt)*
    ) => {
        parse_children!(@match_backwards,
            $parse_args, $iter,
            ({
                let $x = $iter
                    .map(|x| Parsers::$variant($parse_args, x))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter();
                $body
            }),
            $($rest)*
        )
    };
    // Single item pattern
    (@match_forwards,
        $parse_args:expr,
        $iter:expr,
        ($body:expr),
        $variant:ident ($x:pat),
        $($rest:tt)*
    ) => {{
        let p = $iter.next().unwrap();
        let $x = Parsers::$variant($parse_args, p)?;
        parse_children!(@match_forwards,
            $parse_args, $iter,
            ($body),
            $($rest)*
        )
    }};
    // Single item pattern after a variable length one: declare reversed and take from the end
    (@match_backwards,
        $parse_args:expr,
        $iter:expr,
        ($body:expr),
        $variant:ident ($x:pat),
        $($rest:tt)*
    ) => {
        parse_children!(@match_backwards, $parse_args, $iter, ({
            let p = $iter.next_back().unwrap();
            let $x = Parsers::$variant($parse_args, p)?;
            $body
        }), $($rest)*)
    };

    // Check no elements remain
    (@match_forwards, $parse_args:expr, $iter:expr, ($body:expr) $(,)*) => {
        $body
    };
    // After a variable length pattern, everything has already been consumed
    (@match_backwards, $parse_args:expr, $iter:expr, ($body:expr) $(,)*) => {
        $body
    };

    ($parse_args:expr, $iter:expr; [$($args:tt)*] => $body:expr) => {
        parse_children!(@match_forwards,
            $parse_args, $iter,
            ($body),
            $($args)*,
        )
    };
}

macro_rules! make_parser {
    (@children_pattern,
        $varpat:ident,
        ($($acc:tt)*),
        [$variant:ident ($x:pat), $($rest:tt)*]
    ) => (
        make_parser!(@children_pattern,
            $varpat,
            ($($acc)* , Rule::$variant),
            [$($rest)*]
        )
    );
    (@children_pattern,
        $varpat:ident,
        ($($acc:tt)*),
        [$variant:ident ($x:ident).., $($rest:tt)*]
    ) => (
        make_parser!(@children_pattern,
            $varpat,
            ($($acc)* , $varpat..),
            [$($rest)*]
        )
    );
    (@children_pattern,
        $varpat:ident,
        (, $($acc:tt)*), [$(,)*]
    ) => ([$($acc)*]);
    (@children_pattern,
        $varpat:ident,
        ($($acc:tt)*), [$(,)*]
    ) => ([$($acc)*]);

    (@children_filter,
        $varpat:ident,
        [$variant:ident ($x:pat), $($rest:tt)*]
    ) => (
        make_parser!(@children_filter, $varpat, [$($rest)*])
    );
    (@children_filter,
        $varpat:ident,
        [$variant:ident ($x:ident).., $($rest:tt)*]
    ) => (
        $varpat.iter().all(|r| r == &Rule::$variant) &&
        make_parser!(@children_filter, $varpat, [$($rest)*])
    );
    (@children_filter, $varpat:ident, [$(,)*]) => (true);

    (@body,
        ($climbers:expr, $input:expr, $pair:expr),
        rule!(
            $name:ident<$o:ty>;
            $span:ident;
            captured_str!($x:pat) => $body:expr
        )
    ) => ({
        let $span = Span::make($input.clone(), $pair.as_span());
        let $x = $pair.as_str();
        let res: Result<_, String> = try { $body };
        res.map_err(|msg| custom_parse_error(&$pair, msg))
    });
    (@body,
        ($climbers:expr, $input:expr, $pair:expr),
        rule!(
            $name:ident<$o:ty>;
            $span:ident;
            children!( $( [$($args:tt)*] => $body:expr ),* $(,)* )
        )
    ) => ({
        let children_rules: Vec<Rule> = $pair
            .clone()
            .into_inner()
            .map(|p| p.as_rule())
            .collect();

        let $span = Span::make($input.clone(), $pair.as_span());
        #[allow(unused_mut)]
        let mut iter = $pair.clone().into_inner();

        #[allow(unreachable_code)]
        match children_rules.as_slice() {
            $(
                make_parser!(@children_pattern, x, (), [$($args)*,])
                if make_parser!(@children_filter, x, [$($args)*,])
                => {
                    parse_children!(($climbers, $input.clone()), iter;
                        [$($args)*] => {
                            let res: Result<_, String> = try { $body };
                            res.map_err(|msg| custom_parse_error(&$pair, msg))
                        }
                    )
                }
                ,
            )*
            [..] => Err(custom_parse_error(
                &$pair,
                format!("Unexpected children: {:?}", children_rules)
            )),
        }
    });
    (@body,
        ($climbers:expr, $input:expr, $pair:expr),
        rule!(
            $name:ident<$o:ty>;
            prec_climb!(
                $other_rule:ident,
                $_climber:expr,
                $args:pat => $body:expr $(,)*
            )
        )
    ) => ({
        let climber = $climbers.get(&Rule::$name).unwrap();
        climber.climb(
            $pair.clone().into_inner(),
            |p| Parsers::$other_rule(($climbers, $input.clone()), p),
            |l, op, r| {
                let $args = (l?, op, r?);
                let res: Result<_, String> = try { $body };
                res.map_err(|msg| custom_parse_error(&$pair, msg))
            },
        )
    });
    (@body,
        ($($things:tt)*),
        rule!(
            $name:ident<$o:ty>;
            $($args:tt)*
        )
    ) => ({
        make_parser!(@body,
            ($($things)*),
            rule!(
                $name<$o>;
                _span;
                $($args)*
            )
        )
    });
    (@body,
        ($($things:tt)*),
        rule!($name:ident<$o:ty>)
    ) => ({
        Ok(())
    });

    (@construct_climber,
        ($map:expr),
        rule!(
            $name:ident<$o:ty>;
            prec_climb!($other_rule:ident, $climber:expr, $($_rest:tt)* )
        )
    ) => ({
        $map.insert(Rule::$name, $climber)
    });
    (@construct_climber, ($($things:tt)*), $($args:tt)*) => (());

    ($( $submac:ident!( $name:ident<$o:ty> $($args:tt)* ); )*) => (
        struct Parsers;

        impl Parsers {
            $(
            #[allow(non_snake_case, unused_variables, clippy::let_unit_value)]
            fn $name<'a>(
                (climbers, input): (&HashMap<Rule, PrecClimber<Rule>>, Rc<str>),
                pair: Pair<'a, Rule>,
            ) -> ParseResult<$o> {
                make_parser!(@body, (climbers, input, pair),
                               $submac!( $name<$o> $($args)* ))
            }
            )*
        }

        fn construct_precclimbers() -> HashMap<Rule, PrecClimber<Rule>> {
            let mut map = HashMap::new();
            $(
                make_parser!(@construct_climber, (map),
                        $submac!( $name<$o> $($args)* ));
            )*
            map
        }

        struct EntryPoint;

        impl EntryPoint {
            $(
            #[allow(non_snake_case, dead_code)]
            fn $name<'a>(
                input: Rc<str>,
                pair: Pair<'a, Rule>,
            ) -> ParseResult<$o> {
                let climbers = construct_precclimbers();
                Parsers::$name((&climbers, input), pair)
            }
            )*
        }
    );
}

// Trim the shared indent off of a vec of lines, as defined by the Dhall semantics of multiline
// literals.
fn trim_indent(lines: &mut Vec<ParsedText>) {
    let is_indent = |c: char| c == ' ' || c == '\t';

    // There is at least one line so this is safe
    let last_line_head = lines.last().unwrap().head();
    let indent_chars = last_line_head
        .char_indices()
        .take_while(|(_, c)| is_indent(*c));
    let mut min_indent_idx = match indent_chars.last() {
        Some((i, _)) => i,
        // If there is no indent char, then no indent needs to be stripped
        None => return,
    };

    for line in lines.iter() {
        // Ignore empty lines
        if line.is_empty() {
            continue;
        }
        // Take chars from line while they match the current minimum indent.
        let indent_chars = last_line_head[0..=min_indent_idx]
            .char_indices()
            .zip(line.head().chars())
            .take_while(|((_, c1), c2)| c1 == c2);
        match indent_chars.last() {
            Some(((i, _), _)) => min_indent_idx = i,
            // If there is no indent char, then no indent needs to be stripped
            None => return,
        };
    }

    // Remove the shared indent from non-empty lines
    for line in lines.iter_mut() {
        if !line.is_empty() {
            line.head_mut().replace_range(0..=min_indent_idx, "");
        }
    }
}

make_parser! {
    rule!(EOI<()>);

    rule!(simple_label<Label>;
        captured_str!(s) => Label::from(s.trim().to_owned())
    );
    rule!(quoted_label<Label>;
        captured_str!(s) => Label::from(s.trim().to_owned())
    );
    rule!(label<Label>; children!(
        [simple_label(l)] => l,
        [quoted_label(l)] => l,
    ));

    rule!(double_quote_literal<ParsedText>; children!(
        [double_quote_chunk(chunks)..] => {
            chunks.collect()
        }
    ));

    rule!(double_quote_chunk<ParsedTextContents>; children!(
        [interpolation(e)] => {
            InterpolatedTextContents::Expr(e)
        },
        [double_quote_escaped(s)] => {
            InterpolatedTextContents::Text(s)
        },
        [double_quote_char(s)] => {
            InterpolatedTextContents::Text(s.to_owned())
        },
    ));
    rule!(double_quote_escaped<String>;
        captured_str!(s) => {
            match s {
                "\"" => "\"".to_owned(),
                "$" => "$".to_owned(),
                "\\" => "\\".to_owned(),
                "/" => "/".to_owned(),
                "b" => "\u{0008}".to_owned(),
                "f" => "\u{000C}".to_owned(),
                "n" => "\n".to_owned(),
                "r" => "\r".to_owned(),
                "t" => "\t".to_owned(),
                // "uXXXX" or "u{XXXXX}"
                _ => {
                    use std::convert::{TryFrom, TryInto};

                    let s = &s[1..];
                    let s = if &s[0..1] == "{" {
                        &s[1..s.len()-1]
                    } else {
                        &s[0..s.len()]
                    };

                    if s.len() > 8 {
                        Err(format!("Escape sequences can't have more than 8 chars: \"{}\"", s))?
                    }

                    // pad with zeroes
                    let s: String = std::iter::repeat('0')
                        .take(8 - s.len())
                        .chain(s.chars())
                        .collect();

                    // `s` has length 8, so `bytes` has length 4
                    let bytes: &[u8] = &hex::decode(s).unwrap();
                    let i = u32::from_be_bytes(bytes.try_into().unwrap());
                    let c = char::try_from(i).unwrap();
                    match i {
                        0xD800..=0xDFFF => {
                            let c_ecapsed = c.escape_unicode();
                            Err(format!("Escape sequences can't contain surrogate pairs: \"{}\"", c_ecapsed))?
                        },
                        0x0FFFE..=0x0FFFF | 0x1FFFE..=0x1FFFF |
                        0x2FFFE..=0x2FFFF | 0x3FFFE..=0x3FFFF |
                        0x4FFFE..=0x4FFFF | 0x5FFFE..=0x5FFFF |
                        0x6FFFE..=0x6FFFF | 0x7FFFE..=0x7FFFF |
                        0x8FFFE..=0x8FFFF | 0x9FFFE..=0x9FFFF |
                        0xAFFFE..=0xAFFFF | 0xBFFFE..=0xBFFFF |
                        0xCFFFE..=0xCFFFF | 0xDFFFE..=0xDFFFF |
                        0xEFFFE..=0xEFFFF | 0xFFFFE..=0xFFFFF |
                        0x10_FFFE..=0x10_FFFF => {
                            let c_ecapsed = c.escape_unicode();
                            Err(format!("Escape sequences can't contain non-characters: \"{}\"", c_ecapsed))?
                        },
                        _ => {}
                    }
                    std::iter::once(c).collect()
                }
            }
        }
    );
    rule!(double_quote_char<&'a str>;
        captured_str!(s) => s
    );

    rule!(single_quote_literal<ParsedText>; children!(
        [single_quote_continue(lines)] => {
            let newline: ParsedText = "\n".to_string().into();

            let mut lines: Vec<ParsedText> = lines
                .into_iter()
                .rev()
                .map(|l| l.into_iter().rev().collect::<ParsedText>())
                .collect();

            trim_indent(&mut lines);

            lines
                .into_iter()
                .intersperse(newline)
                .flat_map(InterpolatedText::into_iter)
                .collect::<ParsedText>()
        }
    ));
    rule!(single_quote_char<&'a str>;
        captured_str!(s) => s
    );
    rule!(escaped_quote_pair<&'a str>;
        captured_str!(_) => "''"
    );
    rule!(escaped_interpolation<&'a str>;
        captured_str!(_) => "${"
    );
    rule!(interpolation<ParsedExpr>; children!(
        [expression(e)] => e
    ));

    // Returns a vec of lines in reversed order, where each line is also in reversed order.
    rule!(single_quote_continue<Vec<Vec<ParsedTextContents>>>; children!(
        [interpolation(c), single_quote_continue(lines)] => {
            let c = InterpolatedTextContents::Expr(c);
            let mut lines = lines;
            lines.last_mut().unwrap().push(c);
            lines
        },
        [escaped_quote_pair(c), single_quote_continue(lines)] => {
            let mut lines = lines;
            // TODO: don't allocate for every char
            let c = InterpolatedTextContents::Text(c.to_owned());
            lines.last_mut().unwrap().push(c);
            lines
        },
        [escaped_interpolation(c), single_quote_continue(lines)] => {
            let mut lines = lines;
            // TODO: don't allocate for every char
            let c = InterpolatedTextContents::Text(c.to_owned());
            lines.last_mut().unwrap().push(c);
            lines
        },
        [single_quote_char(c), single_quote_continue(lines)] => {
            let mut lines = lines;
            if c == "\n" || c == "\r\n" {
                lines.push(vec![]);
            } else {
                // TODO: don't allocate for every char
                let c = InterpolatedTextContents::Text(c.to_owned());
                lines.last_mut().unwrap().push(c);
            }
            lines
        },
        [] => {
            vec![vec![]]
        },
    ));

    rule!(builtin<ParsedExpr>; span;
        captured_str!(s) => {
            spanned(span, match crate::Builtin::parse(s) {
                Some(b) => Builtin(b),
                None => match s {
                    "True" => BoolLit(true),
                    "False" => BoolLit(false),
                    "Type" => Const(crate::Const::Type),
                    "Kind" => Const(crate::Const::Kind),
                    "Sort" => Const(crate::Const::Sort),
                    _ => Err(
                        format!("Unrecognized builtin: '{}'", s)
                    )?,
                }
            })
        }
    );

    rule!(NaN<()>);
    rule!(minus_infinity_literal<()>);
    rule!(plus_infinity_literal<()>);

    rule!(numeric_double_literal<core::Double>;
        captured_str!(s) => {
            let s = s.trim();
            match s.parse::<f64>() {
                Ok(x) if x.is_infinite() =>
                    Err(format!("Overflow while parsing double literal '{}'", s))?,
                Ok(x) => NaiveDouble::from(x),
                Err(e) => Err(format!("{}", e))?,
            }
        }
    );

    rule!(double_literal<core::Double>; children!(
        [numeric_double_literal(n)] => n,
        [minus_infinity_literal(n)] => std::f64::NEG_INFINITY.into(),
        [plus_infinity_literal(n)] => std::f64::INFINITY.into(),
        [NaN(n)] => std::f64::NAN.into(),
    ));

    rule!(natural_literal<core::Natural>;
        captured_str!(s) => {
            s.trim()
                .parse()
                .map_err(|e| format!("{}", e))?
        }
    );

    rule!(integer_literal<core::Integer>;
        captured_str!(s) => {
            s.trim()
                .parse()
                .map_err(|e| format!("{}", e))?
        }
    );

    rule!(identifier<ParsedExpr>; span; children!(
        [variable(v)] => {
            spanned(span, Var(v))
        },
        [builtin(e)] => e,
    ));

    rule!(variable<V<Label>>; children!(
        [label(l), natural_literal(idx)] => {
            V(l, idx)
        },
        [label(l)] => {
            V(l, 0)
        },
    ));

    rule!(unquoted_path_component<&'a str>; captured_str!(s) => s);
    rule!(quoted_path_component<&'a str>; captured_str!(s) => s);
    rule!(path_component<String>; children!(
        [unquoted_path_component(s)] => s.to_string(),
        [quoted_path_component(s)] => {
            const RESERVED: &percent_encoding::AsciiSet =
                &percent_encoding::CONTROLS
                .add(b'=').add(b':').add(b'/').add(b'?')
                .add(b'#').add(b'[').add(b']').add(b'@')
                .add(b'!').add(b'$').add(b'&').add(b'\'')
                .add(b'(').add(b')').add(b'*').add(b'+')
                .add(b',').add(b';');
            s.chars()
                .map(|c| {
                    // Percent-encode ascii chars
                    if c.is_ascii() {
                        percent_encoding::utf8_percent_encode(
                            &c.to_string(),
                            RESERVED,
                        ).to_string()
                    } else {
                        c.to_string()
                    }
                })
                .collect()
        },
    ));
    rule!(path<Vec<String>>; children!(
        [path_component(components)..] => {
            components.collect()
        }
    ));

    rule!(local<(FilePrefix, Vec<String>)>; children!(
        [parent_path(l)] => l,
        [here_path(l)] => l,
        [home_path(l)] => l,
        [absolute_path(l)] => l,
    ));

    rule!(parent_path<(FilePrefix, Vec<String>)>; children!(
        [path(p)] => (FilePrefix::Parent, p)
    ));
    rule!(here_path<(FilePrefix, Vec<String>)>; children!(
        [path(p)] => (FilePrefix::Here, p)
    ));
    rule!(home_path<(FilePrefix, Vec<String>)>; children!(
        [path(p)] => (FilePrefix::Home, p)
    ));
    rule!(absolute_path<(FilePrefix, Vec<String>)>; children!(
        [path(p)] => (FilePrefix::Absolute, p)
    ));

    rule!(scheme<Scheme>; captured_str!(s) => match s {
        "http" => Scheme::HTTP,
        "https" => Scheme::HTTPS,
        _ => unreachable!(),
    });

    rule!(http_raw<URL<ParsedExpr>>; children!(
        [scheme(sch), authority(auth), path(p)] => URL {
            scheme: sch,
            authority: auth,
            path: p,
            query: None,
            headers: None,
        },
        [scheme(sch), authority(auth), path(p), query(q)] => URL {
            scheme: sch,
            authority: auth,
            path: p,
            query: Some(q),
            headers: None,
        },
    ));

    rule!(authority<String>; captured_str!(s) => s.to_owned());

    rule!(query<String>; captured_str!(s) => s.to_owned());

    rule!(http<URL<ParsedExpr>>; children!(
        [http_raw(url)] => url,
        [http_raw(url), import_expression(e)] =>
            URL { headers: Some(e), ..url },
    ));

    rule!(env<String>; children!(
        [bash_environment_variable(s)] => s,
        [posix_environment_variable(s)] => s,
    ));
    rule!(bash_environment_variable<String>; captured_str!(s) => s.to_owned());
    rule!(posix_environment_variable<String>; children!(
        [posix_environment_variable_character(chars)..] => {
            chars.collect()
        },
    ));
    rule!(posix_environment_variable_character<Cow<'a, str>>;
        captured_str!(s) => {
            match s {
                "\\\"" => Cow::Owned("\"".to_owned()),
                "\\\\" => Cow::Owned("\\".to_owned()),
                "\\a" =>  Cow::Owned("\u{0007}".to_owned()),
                "\\b" =>  Cow::Owned("\u{0008}".to_owned()),
                "\\f" =>  Cow::Owned("\u{000C}".to_owned()),
                "\\n" =>  Cow::Owned("\n".to_owned()),
                "\\r" =>  Cow::Owned("\r".to_owned()),
                "\\t" =>  Cow::Owned("\t".to_owned()),
                "\\v" =>  Cow::Owned("\u{000B}".to_owned()),
                _ => Cow::Borrowed(s)
            }
        }
    );

    rule!(missing<()>);

    rule!(import_type<ImportLocation<ParsedExpr>>; children!(
        [missing(_)] => {
            ImportLocation::Missing
        },
        [env(e)] => {
            ImportLocation::Env(e)
        },
        [http(url)] => {
            ImportLocation::Remote(url)
        },
        [local((prefix, p))] => {
            ImportLocation::Local(prefix, p)
        },
    ));

    rule!(hash<Hash>; captured_str!(s) => {
        let s = s.trim();
        let protocol = &s[..6];
        let hash = &s[7..];
        if protocol != "sha256" {
            Err(format!("Unknown hashing protocol '{}'", protocol))?
        }
        Hash::SHA256(hex::decode(hash).unwrap())
    });

    rule!(import_hashed<crate::Import<ParsedExpr>>; children!(
        [import_type(location)] =>
            crate::Import {mode: ImportMode::Code, location, hash: None },
        [import_type(location), hash(h)] =>
            crate::Import {mode: ImportMode::Code, location, hash: Some(h) },
    ));

    rule!(Text<()>);
    rule!(Location<()>);

    rule!(import<ParsedExpr>; span; children!(
        [import_hashed(imp)] => {
            spanned(span, Import(crate::Import {
                mode: ImportMode::Code,
                ..imp
            }))
        },
        [import_hashed(imp), Text(_)] => {
            spanned(span, Import(crate::Import {
                mode: ImportMode::RawText,
                ..imp
            }))
        },
        [import_hashed(imp), Location(_)] => {
            spanned(span, Import(crate::Import {
                mode: ImportMode::Location,
                ..imp
            }))
        },
    ));

    rule!(lambda<()>);
    rule!(forall<()>);
    rule!(arrow<()>);
    rule!(merge<()>);
    rule!(assert<()>);
    rule!(if_<()>);
    rule!(in_<()>);
    rule!(toMap<()>);

    rule!(empty_list_literal<ParsedExpr>; span; children!(
        [application_expression(e)] => {
            spanned(span, EmptyListLit(e))
        },
    ));

    rule!(expression<ParsedExpr>; span; children!(
        [lambda(()), label(l), expression(typ),
                arrow(()), expression(body)] => {
            spanned(span, Lam(l, typ, body))
        },
        [if_(()), expression(cond), expression(left), expression(right)] => {
            spanned(span, BoolIf(cond, left, right))
        },
        [let_binding(bindings).., in_(()), expression(final_expr)] => {
            bindings.rev().fold(
                final_expr,
                |acc, x| unspanned(Let(x.0, x.1, x.2, acc))
            )
        },
        [forall(()), label(l), expression(typ),
                arrow(()), expression(body)] => {
            spanned(span, Pi(l, typ, body))
        },
        [operator_expression(typ), arrow(()), expression(body)] => {
            spanned(span, Pi("_".into(), typ, body))
        },
        [merge(()), import_expression(x), import_expression(y),
                application_expression(z)] => {
            spanned(span, Merge(x, y, Some(z)))
        },
        [empty_list_literal(e)] => e,
        [assert(()), expression(x)] => {
            spanned(span, Assert(x))
        },
        [toMap(()), import_expression(x), application_expression(y)] => {
            spanned(span, ToMap(x, Some(y)))
        },
        [operator_expression(e)] => e,
        [operator_expression(e), expression(annot)] => {
            spanned(span, Annot(e, annot))
        },
    ));

    rule!(let_binding<(Label, Option<ParsedExpr>, ParsedExpr)>;
            children!(
        [label(name), expression(annot), expression(expr)] =>
            (name, Some(annot), expr),
        [label(name), expression(expr)] =>
            (name, None, expr),
    ));

    rule!(List<()>);
    rule!(Optional<()>);

    rule!(operator_expression<ParsedExpr>; prec_climb!(
        application_expression,
        {
            use Rule::*;
            // In order of precedence
            let operators = vec![
                import_alt,
                bool_or,
                natural_plus,
                text_append,
                list_append,
                bool_and,
                combine,
                prefer,
                combine_types,
                natural_times,
                bool_eq,
                bool_ne,
                equivalent,
            ];
            PrecClimber::new(
                operators
                    .into_iter()
                    .map(|op| pcl::Operator::new(op, pcl::Assoc::Left))
                    .collect(),
            )
        },
        (l, op, r) => {
            use crate::BinOp::*;
            use Rule::*;
            let op = match op.as_rule() {
                import_alt => ImportAlt,
                bool_or => BoolOr,
                natural_plus => NaturalPlus,
                text_append => TextAppend,
                list_append => ListAppend,
                bool_and => BoolAnd,
                combine => RecursiveRecordMerge,
                prefer => RightBiasedRecordMerge,
                combine_types => RecursiveRecordTypeMerge,
                natural_times => NaturalTimes,
                bool_eq => BoolEQ,
                bool_ne => BoolNE,
                equivalent => Equivalence,
                r => Err(
                    format!("Rule {:?} isn't an operator", r),
                )?,
            };

            unspanned(BinOp(op, l, r))
        }
    ));

    rule!(Some_<()>);

    rule!(application_expression<ParsedExpr>; children!(
        [first_application_expression(e)] => e,
        [first_application_expression(first), import_expression(rest)..] => {
            rest.fold(first, |acc, e| unspanned(App(acc, e)))
        },
    ));

    rule!(first_application_expression<ParsedExpr>; span;
            children!(
        [Some_(()), import_expression(e)] => {
            spanned(span, SomeLit(e))
        },
        [merge(()), import_expression(x), import_expression(y)] => {
            spanned(span, Merge(x, y, None))
        },
        [toMap(()), import_expression(x)] => {
            spanned(span, ToMap(x, None))
        },
        [import_expression(e)] => e,
    ));

    rule!(import_expression<ParsedExpr>; span;
            children!(
        [selector_expression(e)] => e,
        [import(e)] => e,
    ));

    rule!(selector_expression<ParsedExpr>; children!(
        [primitive_expression(e)] => e,
        [primitive_expression(first), selector(rest)..] => {
            rest.fold(first, |acc, e| unspanned(match e {
                Either::Left(l) => Field(acc, l),
                Either::Right(ls) => Projection(acc, ls),
            }))
        },
    ));

    rule!(selector<Either<Label, DupTreeSet<Label>>>; children!(
        [label(l)] => Either::Left(l),
        [labels(ls)] => Either::Right(ls),
        [expression(e)] => unimplemented!("selection by expression"), // TODO
    ));

    rule!(labels<DupTreeSet<Label>>; children!(
        [label(ls)..] => ls.collect(),
    ));

    rule!(primitive_expression<ParsedExpr>; span; children!(
        [double_literal(n)] => spanned(span, DoubleLit(n)),
        [natural_literal(n)] => spanned(span, NaturalLit(n)),
        [integer_literal(n)] => spanned(span, IntegerLit(n)),
        [double_quote_literal(s)] => spanned(span, TextLit(s)),
        [single_quote_literal(s)] => spanned(span, TextLit(s)),
        [empty_record_type(e)] => e,
        [empty_record_literal(e)] => e,
        [non_empty_record_type_or_literal(e)] => e,
        [union_type(e)] => e,
        [non_empty_list_literal(e)] => e,
        [identifier(e)] => e,
        [expression(e)] => e,
    ));

    rule!(empty_record_literal<ParsedExpr>; span;
        captured_str!(_) => spanned(span, RecordLit(Default::default()))
    );

    rule!(empty_record_type<ParsedExpr>; span;
        captured_str!(_) => spanned(span, RecordType(Default::default()))
    );

    rule!(non_empty_record_type_or_literal<ParsedExpr>; span;
          children!(
        [label(first_label), non_empty_record_type(rest)] => {
            let (first_expr, mut map) = rest;
            map.insert(first_label, first_expr);
            spanned(span, RecordType(map))
        },
        [label(first_label), non_empty_record_literal(rest)] => {
            let (first_expr, mut map) = rest;
            map.insert(first_label, first_expr);
            spanned(span, RecordLit(map))
        },
    ));

    rule!(non_empty_record_type
          <(ParsedExpr, DupTreeMap<Label, ParsedExpr>)>; children!(
        [expression(expr), record_type_entry(entries)..] => {
            (expr, entries.collect())
        }
    ));

    rule!(record_type_entry<(Label, ParsedExpr)>; children!(
        [label(name), expression(expr)] => (name, expr)
    ));

    rule!(non_empty_record_literal
          <(ParsedExpr, DupTreeMap<Label, ParsedExpr>)>; children!(
        [expression(expr), record_literal_entry(entries)..] => {
            (expr, entries.collect())
        }
    ));

    rule!(record_literal_entry<(Label, ParsedExpr)>; children!(
        [label(name), expression(expr)] => (name, expr)
    ));

    rule!(union_type<ParsedExpr>; span; children!(
        [empty_union_type(_)] => {
            spanned(span, UnionType(Default::default()))
        },
        [union_type_entry(entries)..] => {
            spanned(span, UnionType(entries.collect()))
        },
    ));

    rule!(empty_union_type<()>);

    rule!(union_type_entry<(Label, Option<ParsedExpr>)>; children!(
        [label(name), expression(expr)] => (name, Some(expr)),
        [label(name)] => (name, None),
    ));

    rule!(non_empty_list_literal<ParsedExpr>; span;
          children!(
        [expression(items)..] => spanned(
            span,
            NEListLit(items.collect())
        )
    ));

    rule!(final_expression<ParsedExpr>; children!(
        [expression(e), EOI(_)] => e
    ));
}

pub fn parse_expr(s: &str) -> ParseResult<ParsedExpr> {
    let mut pairs = DhallParser::parse(Rule::final_expression, s)?;
    let rc_input = s.to_string().into();
    let expr = EntryPoint::final_expression(rc_input, pairs.next().unwrap())?;
    assert_eq!(pairs.next(), None);
    Ok(expr)
}
