use crate::app::state::*;
use crate::features::timeline::model::UserDraft;
use rand::Rng;
use std::collections::VecDeque;

const INPUT_PLACEHOLDERS: &[&str] = &[
    "用 /init 生成 AGENTS.md, 先让 omini 认识这个项目",
    "接手陌生项目？让 omini 先画一张结构地图",
    "总结当前改动：做了什么、风险在哪、还缺什么测试",
    "读取 @文件，解释它在整个项目里的职责",
    "读取 @目录，帮我梳理核心模块和调用链",
    "为 @文件 补一组最小但有效的测试",
    "让 @subagent 先调研这个问题，再给我结论",
    "结合 @文件 和 @目录，找出最可能出错的位置",
    "先用 /plan 讨论方案，别急着动代码",
    "用 /agents 创建一个适合当前任务的 subagent",
    "上下文太长了？用 /compact 留下决策和关键线索",
    "用 /help 查看命令、技能和输入技巧",
];

fn pick_input_placeholder() -> String {
    let mut rng = rand::thread_rng();
    INPUT_PLACEHOLDERS[rng.gen_range(0..INPUT_PLACEHOLDERS.len())].to_string()
}

#[derive(Debug)]
pub struct ComposerState {
    pub revision: u64,
    pub local_requests: Vec<crate::platform::files::LocalRequest>,
    pub cwd: std::path::PathBuf,
    pub input: String,
    pub input_placeholder: String,
    pub input_mentions: Vec<InputMention>,
    pub input_images: Vec<InputImageAttachment>,
    pub input_paste_markers: Vec<InputPasteMarker>,
    pub input_scroll_line: usize,
    pub input_wrap_width: usize,
    /// 当前 query 运行期间由普通 Enter 暂存在 UI 侧的用户输入。
    pub queued_user_inputs: VecDeque<UserDraft>,
    /// 已提交给 engine、等待当前轮结束后插入历史的用户输入。
    pub pending_intervention_inputs: VecDeque<UserDraft>,
    /// 当前 intervention 请求对应的 optimistic echo token。
    pub pending_intervention_client_echo_id: Option<String>,
    /// 光标偏移量，按 Unicode 字符计数（不是字节）
    pub cursor_char: usize,
    /// 命令自动补全
    pub autocomplete: CommandAutocomplete,
    /// @ mention 自动补全
    pub mention_autocomplete: MentionAutocomplete,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            revision: 0,
            local_requests: Vec::new(),
            cwd: Default::default(),
            input: String::new(),
            input_placeholder: pick_input_placeholder(),
            input_mentions: Vec::new(),
            input_images: Vec::new(),
            input_paste_markers: Vec::new(),
            input_scroll_line: 0,
            input_wrap_width: DEFAULT_INPUT_WRAP_WIDTH,
            queued_user_inputs: VecDeque::new(),
            pending_intervention_inputs: VecDeque::new(),
            pending_intervention_client_echo_id: None,
            cursor_char: 0,
            autocomplete: CommandAutocomplete {
                all_commands: crate::features::commands::builtin_command_summaries(),
                ..CommandAutocomplete::default()
            },
            mention_autocomplete: MentionAutocomplete::default(),
        }
    }
}

impl ComposerState {
    /// 新会话开始时更新空输入提示；编辑内容及附件仍由会话切换流程处理。
    pub fn refresh_input_placeholder(&mut self) {
        self.input_placeholder = pick_input_placeholder();
    }
}
