use crate::app::event::CommandSummary;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HelpTab {
    #[default]
    General,
    Commands,
    Skills,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpDrawerState {
    pub tab: HelpTab,
    pub commands: Vec<CommandSummary>,
    pub general_selected: usize,
    pub command_selected: usize,
    pub skill_selected: usize,
}

impl HelpDrawerState {
    pub fn new(commands: Vec<CommandSummary>) -> Self {
        Self {
            tab: HelpTab::General,
            commands,
            general_selected: 0,
            command_selected: 0,
            skill_selected: 0,
        }
    }
}
