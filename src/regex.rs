use std::collections::HashSet;

// ─── Regex AST Definition ────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegexAST {
    Char(char),
    Wildcard,                  // .
    Digit,                     // \d
    Alphanumeric,              // \w
    Whitespace,                // \s
    NonDigit,                  // \D
    NonAlphanumeric,           // \W
    NonWhitespace,             // \S
    Concat(Vec<RegexAST>),
    Alternate(Box<RegexAST>, Box<RegexAST>), // a|b
    ZeroOrMore(Box<RegexAST>),               // *
    OneOrMore(Box<RegexAST>),                // +
    ZeroOrOne(Box<RegexAST>),                // ?
    Class {
        chars: Vec<char>,
        ranges: Vec<(char, char)>,
        negated: bool,
    },
}

// ─── Recursive Descent Parser ────────────────────────────────────────────

pub struct Parser<'a> {
    input: Vec<char>,
    pos: usize,
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> Parser<'a> {
    pub fn new(pattern: &str) -> Self {
        Self {
            input: pattern.chars().collect(),
            pos: 0,
            _phantom: std::marker::PhantomData,
        }
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    pub fn parse(&mut self) -> Result<RegexAST, String> {
        if self.input.is_empty() {
            return Ok(RegexAST::Concat(Vec::new()));
        }
        let ast = self.parse_regex()?;
        if self.pos < self.input.len() {
            return Err(format!(
                "Unexpected character at index {}: '{}'",
                self.pos, self.input[self.pos]
            ));
        }
        Ok(ast)
    }

    fn parse_regex(&mut self) -> Result<RegexAST, String> {
        let mut term = self.parse_term()?;
        while self.peek() == Some('|') {
            self.next(); // Consume '|'
            let next_term = self.parse_term()?;
            term = RegexAST::Alternate(Box::new(term), Box::new(next_term));
        }
        Ok(term)
    }

    fn parse_term(&mut self) -> Result<RegexAST, String> {
        let mut factors = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            factors.push(self.parse_factor()?);
        }
        if factors.len() == 1 {
            Ok(factors.remove(0))
        } else {
            Ok(RegexAST::Concat(factors))
        }
    }

    fn parse_factor(&mut self) -> Result<RegexAST, String> {
        let base = self.parse_base()?;
        if let Some(c) = self.peek() {
            if c == '*' {
                self.next();
                return Ok(RegexAST::ZeroOrMore(Box::new(base)));
            } else if c == '+' {
                self.next();
                return Ok(RegexAST::OneOrMore(Box::new(base)));
            } else if c == '?' {
                self.next();
                return Ok(RegexAST::ZeroOrOne(Box::new(base)));
            } else if c == '{' {
                self.next(); // Consume '{'
                let mut num_str = String::new();
                while let Some(nc) = self.peek() {
                    if nc.is_ascii_digit() {
                        num_str.push(self.next().unwrap());
                    } else {
                        break;
                    }
                }
                
                if self.peek() == Some(',') {
                    self.next(); // Consume ','
                    let mut max_str = String::new();
                    while let Some(nc) = self.peek() {
                        if nc.is_ascii_digit() {
                            max_str.push(self.next().unwrap());
                        } else {
                            break;
                        }
                    }
                    if self.next() != Some('}') {
                        return Err("Unclosed quantifier bounds '{min,max}'".to_string());
                    }
                    let min: usize = num_str.parse().map_err(|_| "Invalid min bound".to_string())?;
                    if max_str.is_empty() {
                        // e.g. {min,} -> min concatenated copies + a Kleene loop
                        let mut elements = Vec::new();
                        for _ in 0..min {
                            elements.push(base.clone());
                        }
                        elements.push(RegexAST::ZeroOrMore(Box::new(base)));
                        return Ok(RegexAST::Concat(elements));
                    } else {
                        let max: usize = max_str.parse().map_err(|_| "Invalid max bound".to_string())?;
                        if max < min {
                            return Err("Max bound cannot be less than min".to_string());
                        }
                        let mut elements = Vec::new();
                        for _ in 0..min {
                            elements.push(base.clone());
                        }
                        for _ in min..max {
                            elements.push(RegexAST::ZeroOrOne(Box::new(base.clone())));
                        }
                        return Ok(RegexAST::Concat(elements));
                    }
                } else {
                    if self.next() != Some('}') {
                        return Err("Unclosed quantifier bound '{n}'".to_string());
                    }
                    let n: usize = num_str.parse().map_err(|_| "Invalid bound integer".to_string())?;
                    let mut elements = Vec::new();
                    for _ in 0..n {
                        elements.push(base.clone());
                    }
                    return Ok(RegexAST::Concat(elements));
                }
            }
        }
        Ok(base)
    }

    fn parse_base(&mut self) -> Result<RegexAST, String> {
        let c = self.next().ok_or_else(|| "Unexpected end of pattern".to_string())?;
        match c {
            '.' => Ok(RegexAST::Wildcard),
            '\\' => {
                let ec = self.next().ok_or_else(|| "Dangling backslash".to_string())?;
                match ec {
                    'd' => Ok(RegexAST::Digit),
                    'w' => Ok(RegexAST::Alphanumeric),
                    's' => Ok(RegexAST::Whitespace),
                    'D' => Ok(RegexAST::NonDigit),
                    'W' => Ok(RegexAST::NonAlphanumeric),
                    'S' => Ok(RegexAST::NonWhitespace),
                    escaped => Ok(RegexAST::Char(escaped)),
                }
            }
            '[' => {
                let negated = if self.peek() == Some('^') {
                    self.next();
                    true
                } else {
                    false
                };

                let mut chars = Vec::new();
                let mut ranges = Vec::new();

                while let Some(nc) = self.peek() {
                    if nc == ']' {
                        break;
                    }
                    let mut cur = self.next().unwrap();
                    if cur == '\\' {
                        cur = self.next().ok_or_else(|| "Dangling backslash inside class".to_string())?;
                    }
                    
                    if self.peek() == Some('-') {
                        self.next(); // Consume '-'
                        let next_c = self.peek().ok_or_else(|| "Dangling range '-'".to_string())?;
                        if next_c == ']' {
                            // Treat '-' as a literal character since it's at the end of class
                            chars.push(cur);
                            chars.push('-');
                        } else {
                            let mut end = self.next().unwrap();
                            if end == '\\' {
                                end = self.next().ok_or_else(|| "Dangling backslash inside class range".to_string())?;
                            }
                            ranges.push((cur, end));
                        }
                    } else {
                        chars.push(cur);
                    }
                }

                if self.next() != Some(']') {
                    return Err("Unclosed character class bracket".to_string());
                }

                Ok(RegexAST::Class { chars, ranges, negated })
            }
            '(' => {
                let inner = self.parse_regex()?;
                if self.next() != Some(')') {
                    return Err("Unclosed parenthesis group".to_string());
                }
                Ok(inner)
            }
            '*' | '+' | '?' | '{' | '}' | ')' | ']' => {
                Err(format!("Dangling operator/grouping symbol: '{}'", c))
            }
            literal => Ok(RegexAST::Char(literal)),
        }
    }
}

// ─── NFA States & Transition Types ───────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    Char(char),
    Wildcard,
    Digit,
    Alphanumeric,
    Whitespace,
    NonDigit,
    NonAlphanumeric,
    NonWhitespace,
    Class {
        chars: Vec<char>,
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    Epsilon,
}

#[derive(Clone, Debug)]
pub struct State {
    pub id: usize,
    pub transitions: Vec<(Transition, usize)>,
}

#[derive(Clone, Debug)]
pub struct Nfa {
    pub states: Vec<State>,
    pub start: usize,
    pub accept: usize,
    pub pattern: String,
}

struct NfaBuilder {
    states: Vec<State>,
}

impl NfaBuilder {
    fn new() -> Self {
        Self { states: Vec::new() }
    }

    fn add_state(&mut self) -> usize {
        let id = self.states.len();
        self.states.push(State {
            id,
            transitions: Vec::new(),
        });
        id
    }

    fn add_trans(&mut self, from: usize, trans: Transition, to: usize) {
        if let Some(state) = self.states.get_mut(from) {
            state.transitions.push((trans, to));
        }
    }

    fn compile_ast(&mut self, ast: &RegexAST) -> Result<(usize, usize), String> {
        match ast {
            RegexAST::Char(c) => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::Char(*c), accept);
                Ok((start, accept))
            }
            RegexAST::Wildcard => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::Wildcard, accept);
                Ok((start, accept))
            }
            RegexAST::Digit => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::Digit, accept);
                Ok((start, accept))
            }
            RegexAST::Alphanumeric => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::Alphanumeric, accept);
                Ok((start, accept))
            }
            RegexAST::Whitespace => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::Whitespace, accept);
                Ok((start, accept))
            }
            RegexAST::NonDigit => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::NonDigit, accept);
                Ok((start, accept))
            }
            RegexAST::NonAlphanumeric => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::NonAlphanumeric, accept);
                Ok((start, accept))
            }
            RegexAST::NonWhitespace => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(start, Transition::NonWhitespace, accept);
                Ok((start, accept))
            }
            RegexAST::Class { chars, ranges, negated } => {
                let start = self.add_state();
                let accept = self.add_state();
                self.add_trans(
                    start,
                    Transition::Class {
                        chars: chars.clone(),
                        ranges: ranges.clone(),
                        negated: *negated,
                    },
                    accept,
                );
                Ok((start, accept))
            }
            RegexAST::Concat(elements) => {
                if elements.is_empty() {
                    let start = self.add_state();
                    let accept = self.add_state();
                    self.add_trans(start, Transition::Epsilon, accept);
                    return Ok((start, accept));
                }

                let mut current = self.compile_ast(&elements[0])?;
                let start = current.0;

                for elem in elements.iter().skip(1) {
                    let next = self.compile_ast(elem)?;
                    self.add_trans(current.1, Transition::Epsilon, next.0);
                    current = next;
                }
                
                Ok((start, current.1))
            }
            RegexAST::Alternate(a, b) => {
                let start = self.add_state();
                let accept = self.add_state();
                
                let nfa_a = self.compile_ast(a)?;
                let nfa_b = self.compile_ast(b)?;
                
                self.add_trans(start, Transition::Epsilon, nfa_a.0);
                self.add_trans(start, Transition::Epsilon, nfa_b.0);
                
                self.add_trans(nfa_a.1, Transition::Epsilon, accept);
                self.add_trans(nfa_b.1, Transition::Epsilon, accept);
                
                Ok((start, accept))
            }
            RegexAST::ZeroOrMore(a) => {
                let start = self.add_state();
                let accept = self.add_state();
                
                let nfa_a = self.compile_ast(a)?;
                
                self.add_trans(start, Transition::Epsilon, nfa_a.0);
                self.add_trans(start, Transition::Epsilon, accept);
                
                self.add_trans(nfa_a.1, Transition::Epsilon, nfa_a.0);
                self.add_trans(nfa_a.1, Transition::Epsilon, accept);
                
                Ok((start, accept))
            }
            RegexAST::OneOrMore(a) => {
                let start = self.add_state();
                let accept = self.add_state();
                
                let nfa_a = self.compile_ast(a)?;
                
                self.add_trans(start, Transition::Epsilon, nfa_a.0);
                self.add_trans(nfa_a.1, Transition::Epsilon, nfa_a.0);
                self.add_trans(nfa_a.1, Transition::Epsilon, accept);
                
                Ok((start, accept))
            }
            RegexAST::ZeroOrOne(a) => {
                let start = self.add_state();
                let accept = self.add_state();
                
                let nfa_a = self.compile_ast(a)?;
                
                self.add_trans(start, Transition::Epsilon, nfa_a.0);
                self.add_trans(start, Transition::Epsilon, accept);
                self.add_trans(nfa_a.1, Transition::Epsilon, accept);
                
                Ok((start, accept))
            }
        }
    }
}

/// Automatically expands a query term into a Levenshtein regex pattern
/// representing 1-character edits and transpositions.
/// For example, "htis" -> "htis|this|tis|his|h.is|ht.s" (and other 1-edit/swap variants).
pub fn levenshtein_regex(term: &str) -> String {
    let chars: Vec<char> = term.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    
    let mut patterns = std::collections::HashSet::new();
    
    // 1. Exact term
    patterns.insert(term.to_string());
    
    // 2. Deletions (distance 1)
    for i in 0..n {
        let mut p = Vec::with_capacity(n - 1);
        p.extend_from_slice(&chars[..i]);
        p.extend_from_slice(&chars[i+1..]);
        let s: String = p.into_iter().collect();
        if !s.is_empty() {
            patterns.insert(s);
        }
    }
    
    // 3. Substitutions with wildcard '.' (distance 1)
    for i in 0..n {
        let mut p = chars.clone();
        p[i] = '.';
        patterns.insert(p.into_iter().collect());
    }
    
    // 4. Insertions of wildcard '.' (distance 1)
    for i in 0..=n {
        let mut p = Vec::with_capacity(n + 1);
        p.extend_from_slice(&chars[..i]);
        p.push('.');
        p.extend_from_slice(&chars[i..]);
        patterns.insert(p.into_iter().collect());
    }
    
    // 5. Transpositions (adjacent character swaps, distance 2/1 transposition)
    for i in 0..n-1 {
        let mut p = chars.clone();
        p.swap(i, i + 1);
        patterns.insert(p.into_iter().collect());
    }
    
    let mut sorted_patterns: Vec<String> = patterns.into_iter().collect();
    sorted_patterns.sort();
    sorted_patterns.join("|")
}

impl Nfa {
    pub fn compile(pattern: &str) -> Result<Self, String> {
        let mut parser = Parser::new(pattern);
        let ast = parser.parse()?;
        
        let mut builder = NfaBuilder::new();
        let (start, accept) = builder.compile_ast(&ast)?;
        
        Ok(Self {
            states: builder.states,
            start,
            accept,
            pattern: pattern.to_string(),
        })
    }

    // ─── Active-Set Linear-Time Match Simulator ──────────────────────────

    pub fn matches(&self, text: &str) -> Vec<(usize, usize)> {
        let mut results = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        
        // Find matching spans starting at any character position in the text
        for start_idx in 0..chars.len() {
            let mut active = HashSet::new();
            active.insert(self.start);
            active = self.epsilon_closure(&active);
            
            let mut longest_match = None;
            
            // Check if empty match is accepted initially
            if active.contains(&self.accept) {
                longest_match = Some(start_idx);
            }
            
            let mut current_idx = start_idx;
            while current_idx < chars.len() {
                let c = chars[current_idx];
                active = self.step_active_set(&active, c);
                if active.is_empty() {
                    break;
                }
                current_idx += 1;
                
                if active.contains(&self.accept) {
                    longest_match = Some(current_idx);
                }
            }
            
            if let Some(end_idx) = longest_match {
                // Return start/end char indices in character counts
                results.push((start_idx, end_idx));
            }
        }
        results
    }

    fn epsilon_closure(&self, states: &HashSet<usize>) -> HashSet<usize> {
        let mut closure = states.clone();
        let mut queue: Vec<usize> = states.iter().cloned().collect();
        
        while let Some(state_id) = queue.pop() {
            if let Some(state) = self.states.get(state_id) {
                for (trans, target) in &state.transitions {
                    if let Transition::Epsilon = trans {
                        if closure.insert(*target) {
                            queue.push(*target);
                        }
                    }
                }
            }
        }
        closure
    }

    fn step_active_set(&self, active: &HashSet<usize>, c: char) -> HashSet<usize> {
        let mut next_states = HashSet::new();
        for &state_id in active {
            if let Some(state) = self.states.get(state_id) {
                for (trans, target) in &state.transitions {
                    if match_transition(trans, c) {
                        next_states.insert(*target);
                    }
                }
            }
        }
        self.epsilon_closure(&next_states)
    }
}

fn match_transition(trans: &Transition, c: char) -> bool {
    match trans {
        Transition::Char(tc) => *tc == c,
        Transition::Wildcard => c != '\n',
        Transition::Digit => c.is_ascii_digit(),
        Transition::Alphanumeric => c.is_alphanumeric() || c == '_',
        Transition::Whitespace => c.is_whitespace(),
        Transition::NonDigit => !c.is_ascii_digit(),
        Transition::NonAlphanumeric => !(c.is_alphanumeric() || c == '_'),
        Transition::NonWhitespace => !c.is_whitespace(),
        Transition::Class { chars, ranges, negated } => {
            let mut found = false;
            if chars.contains(&c) {
                found = true;
            } else {
                for (start, end) in ranges {
                    if c >= *start && c <= *end {
                        found = true;
                        break;
                    }
                }
            }
            if *negated {
                !found
            } else {
                found
            }
        }
        Transition::Epsilon => false,
    }
}

// ─── Regex Unit Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_literal_match() {
        let nfa = Nfa::compile("hello").unwrap();
        let matches = nfa.matches("hello world");
        assert_eq!(matches, vec![(0, 5)]);
    }

    #[test]
    fn test_wildcard_match() {
        let nfa = Nfa::compile("h.llo").unwrap();
        let matches = nfa.matches("hello hillo");
        assert_eq!(matches, vec![(0, 5), (6, 11)]);
    }

    #[test]
    fn test_digit_and_alphanumeric() {
        let nfa = Nfa::compile("SKU-\\d\\d\\d").unwrap();
        let matches = nfa.matches("item SKU-123 is here");
        assert_eq!(matches, vec![(5, 12)]);
    }

    #[test]
    fn test_quantifiers() {
        // Star *
        let nfa = Nfa::compile("ab*c").unwrap();
        assert_eq!(nfa.matches("ac abc abbc"), vec![(0, 2), (3, 6), (7, 11)]);

        // Plus +
        let nfa = Nfa::compile("ab+c").unwrap();
        assert_eq!(nfa.matches("ac abc abbc"), vec![(3, 6), (7, 11)]);

        // Question ?
        let nfa = Nfa::compile("ab?c").unwrap();
        assert_eq!(nfa.matches("ac abc abbc"), vec![(0, 2), (3, 6)]);
    }

    #[test]
    fn test_repetition_bounds() {
        // Bound {n}
        let nfa = Nfa::compile("a{3}").unwrap();
        assert_eq!(nfa.matches("aa aaa aaaa"), vec![(3, 6), (7, 10), (8, 11)]);

        // Bound {min,}
        let nfa = Nfa::compile("a{2,}").unwrap();
        assert_eq!(nfa.matches("a aa aaa"), vec![(2, 4), (5, 8), (6, 8)]); // "aaa" matches full 3 chars at pos 5, and "aa" starts at pos 6

        // Bound {min,max}
        let nfa = Nfa::compile("a{2,3}").unwrap();
        assert_eq!(
            nfa.matches("a aa aaa aaaa"),
            vec![(2, 4), (5, 8), (6, 8), (9, 12), (10, 13), (11, 13)]
        );
    }

    #[test]
    fn test_alternation_and_grouping() {
        let nfa = Nfa::compile("(abc|def)g").unwrap();
        assert_eq!(nfa.matches("abcg defg abcd"), vec![(0, 4), (5, 9)]);
    }

    #[test]
    fn test_character_class() {
        let nfa = Nfa::compile("[A-Za-z0-9]+").unwrap();
        assert_eq!(
            nfa.matches("Hello123!"),
            vec![(0, 8), (1, 8), (2, 8), (3, 8), (4, 8), (5, 8), (6, 8), (7, 8)]
        );
        
        let nfa_neg = Nfa::compile("[^a-z]").unwrap();
        assert_eq!(nfa_neg.matches("a B c"), vec![(1, 2), (2, 3), (3, 4)]);
    }

    #[test]
    fn test_levenshtein_regex_expansion() {
        let pattern = levenshtein_regex("htis");
        
        // Assert that the generated pattern contains key illustrative edits
        assert!(pattern.contains("htis"));
        assert!(pattern.contains("this"));
        assert!(pattern.contains("tis"));
        assert!(pattern.contains("his"));
        assert!(pattern.contains("h.is"));
        assert!(pattern.contains("ht.s"));
        
        let nfa = Nfa::compile(&pattern).unwrap();
        
        // The NFA should match all these spelling-corrected variants
        assert!(!nfa.matches("this").is_empty());
        assert!(!nfa.matches("tis").is_empty());
        assert!(!nfa.matches("his").is_empty());
        assert!(!nfa.matches("htis").is_empty());
    }
}
