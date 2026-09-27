//! 基于 Bash AST 的命令分析；只检查可见执行，不推断脚本或动态表达式的实际行为。

use tree_sitter::{Node, Parser};

/// 动态参数仍占一个位置，避免删除它后让后续字面量错误匹配前缀规则。
#[derive(Debug, Clone)]
pub(crate) struct ShellWord {
    pub literal: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ShellCommand {
    pub words: Vec<ShellWord>,
}

impl ShellCommand {
    pub fn name(&self) -> Option<&str> {
        self.words.first()?.literal.as_deref()
    }

    pub fn basename(&self) -> Option<&str> {
        self.name()?.rsplit('/').next()
    }
}

#[derive(Debug, Default)]
pub(crate) struct ShellAnalysis {
    pub commands: Vec<ShellCommand>,
    pub incomplete: bool,
    pub fork_bomb: bool,
}

/// 分析所有可见命令及字面量包装；失败与超限只标记不完整，不丢弃已识别的风险。
pub(crate) fn analyze_shell(source: &str) -> ShellAnalysis {
    let mut analysis = ShellAnalysis::default();
    analyze_source(source, 0, &mut analysis);
    analysis
}

const MAX_WRAPPER_DEPTH: usize = 8;

fn analyze_source(source: &str, depth: usize, analysis: &mut ShellAnalysis) {
    if depth > MAX_WRAPPER_DEPTH {
        analysis.incomplete = true;
        return;
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        analysis.incomplete = true;
        return;
    }
    let Some(tree) = parser.parse(source, None) else {
        analysis.incomplete = true;
        return;
    };
    analysis.incomplete |= tree.root_node().has_error();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition"
            && node
                .child_by_field_name("name")
                .is_some_and(|name| text(name, source) == ":")
            && let Some(body) = node.child_by_field_name("body")
        {
            let compact: String = text(body, source)
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect();
            analysis.fork_bomb |= matches!(compact.as_str(), "{:|:&}" | "{:|:&;}");
        }
        if node.kind() == "command"
            && let Some(name) = node.child_by_field_name("name")
        {
            let mut words = vec![parse_word(name, source)];
            let mut cursor = node.walk();
            words.extend(
                node.children_by_field_name("argument", &mut cursor)
                    .map(|arg| parse_word(arg, source)),
            );
            analyze_command(ShellCommand { words }, depth, analysis);
        }
        // 引用文本、注释和 heredoc 文本没有 command 节点；实际命令替换仍会被遍历。
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn parse_word(node: Node<'_>, source: &str) -> ShellWord {
    ShellWord {
        literal: literal_word(node, source),
        source: text(node, source).to_string(),
    }
}

/// 只解码无展开的 shell 单词；引号和转义保持 shell 的字面量语义。
fn literal_word(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "command_name" => literal_word(node.named_child(0)?, source),
        "raw_string" => Some(
            text(node, source)
                .strip_prefix('\'')?
                .strip_suffix('\'')?
                .to_string(),
        ),
        "concatenation" => {
            let mut value = String::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                value.push_str(&literal_word(child, source)?);
            }
            Some(value)
        }
        "word" | "number" | "string" => {
            let mut cursor = node.walk();
            if node
                .named_children(&mut cursor)
                .any(|child| child.kind() != "string_content")
            {
                return None;
            }
            decode_word(text(node, source))
        }
        _ => None,
    }
}

fn decode_word(source: &str) -> Option<String> {
    let mut result = String::new();
    let mut quote = None;
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => result.push(ch),
            (None, '\'' | '"') => quote = Some(ch),
            (_, '\\') => {
                let next = chars.next()?;
                if next == '\n' {
                    continue;
                }
                if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\') {
                    result.push('\\');
                }
                result.push(next);
            }
            (_, '$' | '`') => return None,
            (None, '*' | '?' | '[' | ']' | '{' | '}' | '~') => return None,
            _ => result.push(ch),
        }
    }
    quote.is_none().then_some(result)
}

fn analyze_command(command: ShellCommand, depth: usize, analysis: &mut ShellAnalysis) {
    if depth > MAX_WRAPPER_DEPTH {
        analysis.incomplete = true;
        return;
    }
    analysis.commands.push(command.clone());
    let Some(name) = command.basename() else {
        return;
    };
    let words = &command.words;
    match name {
        "sh" | "bash" | "zsh" => {
            for (index, word) in words.iter().enumerate().skip(1) {
                let Some(flag) = word.literal.as_deref() else {
                    break;
                };
                if flag == "--" || !flag.starts_with('-') {
                    break;
                }
                if !flag.starts_with("--") && flag.contains('c') {
                    if let Some(script) = words
                        .get(index + 1)
                        .and_then(|word| word.literal.as_deref())
                    {
                        analyze_source(script, depth + 1, analysis);
                    }
                    break;
                }
            }
        }
        "eval" => {
            if let Some(parts) = words[1..]
                .iter()
                .map(|word| word.literal.as_deref())
                .collect::<Option<Vec<_>>>()
            {
                analyze_source(&parts.join(" "), depth + 1, analysis);
            }
        }
        "trap" => {
            let index = if words.get(1).and_then(|word| word.literal.as_deref()) == Some("--") {
                2
            } else {
                1
            };
            if let Some(action) = words.get(index).and_then(|word| word.literal.as_deref())
                && !action.starts_with('-')
            {
                analyze_source(action, depth + 1, analysis);
            }
        }
        "env" | "command" | "exec" | "sudo" | "doas" => {
            // env -S 使用独立分词语法；静态内容无法完整分析时询问，动态内容仍不猜测。
            if name == "env"
                && let Some(script) = env_split(words)
            {
                analysis.incomplete |= script.literal.is_some();
                return;
            }
            if let Some(index) = unwrap_command(name, words) {
                analyze_command(
                    ShellCommand {
                        words: words[index..].to_vec(),
                    },
                    depth + 1,
                    analysis,
                );
            }
        }
        "su" => {
            if let Some(index) = words
                .iter()
                .position(|word| matches!(word.literal.as_deref(), Some("-c" | "--command")))
                && let Some(script) = words
                    .get(index + 1)
                    .and_then(|word| word.literal.as_deref())
            {
                analyze_source(script, depth + 1, analysis);
            }
        }
        _ => {}
    }
}

/// 只检查包装器选项，避免把实际子命令的 -S 参数误认为 env 分词选项。
fn env_split(words: &[ShellWord]) -> Option<&ShellWord> {
    let mut index = 1;
    while let Some(word) = words.get(index) {
        if is_assignment(&word.source) {
            index += 1;
            continue;
        }
        let arg = word.literal.as_deref()?;
        if matches!(arg, "-S" | "--split-string") {
            return words.get(index + 1);
        }
        if arg.starts_with("--split-string=") || arg.starts_with("-S") {
            return Some(word);
        }
        if arg == "--" || !arg.starts_with('-') {
            break;
        }
        index += if matches!(arg, "-u" | "--unset" | "-C" | "--chdir") {
            2
        } else {
            1
        };
    }
    None
}

/// 剥离包装器的选项与环境赋值；查询形式不执行参数，动态形式不猜测命令位置。
fn unwrap_command(name: &str, words: &[ShellWord]) -> Option<usize> {
    let mut index = 1;
    while let Some(word) = words.get(index) {
        let arg = word.literal.as_deref();
        if name == "env" && is_assignment(&word.source) {
            index += 1;
            continue;
        }
        let arg = arg?;
        if arg == "--" {
            return (index + 1 < words.len()).then_some(index + 1);
        }
        if !arg.starts_with('-') || arg == "-" {
            return Some(index);
        }
        if name == "command" && matches!(arg, "-v" | "-V") {
            return None;
        }
        let consumes_value = match name {
            "env" => matches!(
                arg,
                "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"
            ),
            "exec" => arg == "-a",
            "sudo" | "doas" => matches!(
                arg,
                "-u" | "-g"
                    | "-h"
                    | "-p"
                    | "-C"
                    | "-T"
                    | "-r"
                    | "-t"
                    | "-D"
                    | "--user"
                    | "--group"
                    | "--host"
                    | "--prompt"
                    | "--chdir"
            ),
            _ => false,
        };
        // env -S 本身是另一个命令语言，不把其字符串当成普通 argv。
        if name == "env" && matches!(arg, "-S" | "--split-string") {
            return None;
        }
        index += if consumes_value { 2 } else { 1 };
    }
    None
}

/// 内置子命令风险匹配忽略已知 CLI 全局选项；显式用户规则仍检查完整原始 argv。
pub(crate) fn risk_words(command: &ShellCommand) -> Vec<ShellWord> {
    let words = &command.words;
    let Some(name) = command.basename() else {
        return words.clone();
    };
    if !matches!(name, "git" | "jj" | "gh" | "docker") {
        return words.clone();
    }
    let mut index = 1;
    while let Some(arg) = words.get(index).and_then(|word| word.literal.as_deref()) {
        if arg == "--" {
            index += 1;
            break;
        }
        if !arg.starts_with('-') {
            break;
        }
        let consumes_value = match name {
            "git" => matches!(
                arg,
                "-C" | "-c"
                    | "--git-dir"
                    | "--work-tree"
                    | "--namespace"
                    | "--config-env"
                    | "--super-prefix"
            ),
            "jj" => matches!(
                arg,
                "-R" | "--repository"
                    | "--at-operation"
                    | "--at-op"
                    | "--color"
                    | "--config"
                    | "--config-file"
            ),
            "gh" => matches!(arg, "-R" | "--repo" | "--hostname"),
            "docker" => matches!(
                arg,
                "-H" | "--host"
                    | "--context"
                    | "--config"
                    | "-l"
                    | "--log-level"
                    | "--tlscacert"
                    | "--tlscert"
                    | "--tlskey"
            ),
            _ => false,
        };
        index += if consumes_value { 2 } else { 1 };
    }
    std::iter::once(words[0].clone())
        .chain(words.get(index..).unwrap_or_default().iter().cloned())
        .collect()
}

fn is_assignment(source: &str) -> bool {
    source.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().enumerate().all(|(index, ch)| {
                ch == '_' || ch.is_ascii_alphabetic() || index > 0 && ch.is_ascii_digit()
            })
    })
}

/// 仅用于规则 DSL 的 match/not_match 示例分词，不参与执行命令分析。
/// 将 shell 命令拆分为参数列表，正确处理引号和转义。
/// 引号字符本身不会出现在结果中（被剥离）。
pub(crate) fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            if ch == '\\' && q == '"' {
                escaped = true;
            } else if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                words.push(current.clone());
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}
