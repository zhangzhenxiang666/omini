use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, stderr};

pub type OminiTerminal = Terminal<CrosstermBackend<io::Stderr>>;

pub fn init() -> io::Result<OminiTerminal> {
    enable_raw_mode()?;
    execute!(stderr(), EnterAlternateScreen)?;
    execute!(stderr(), EnableBracketedPaste)?;
    execute!(
        stderr(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
        )
    )?;
    execute!(stderr(), EnableMouseCapture)?;
    execute!(stderr(), Hide)?;
    Terminal::new(CrosstermBackend::new(stderr()))
}

pub fn safe_restore() {
    let _ = execute!(io::stderr(), Show);
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), PopKeyboardEnhancementFlags);
    let _ = execute!(io::stderr(), DisableBracketedPaste);
    let _ = execute!(io::stderr(), LeaveAlternateScreen);
    let _ = execute!(io::stderr(), DisableMouseCapture);
}

pub fn restore(terminal: &mut OminiTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

/// 即使初始化、绘制或网络操作失败，也恢复终端模式。
pub struct RestoreGuard;
impl Default for RestoreGuard {
    fn default() -> Self {
        Self::new()
    }
}
impl RestoreGuard {
    pub fn new() -> Self {
        Self
    }
}
impl Drop for RestoreGuard {
    fn drop(&mut self) {
        safe_restore();
    }
}
