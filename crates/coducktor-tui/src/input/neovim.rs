use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalCommand {
    Motion(Direction),
    Window(Direction),
    WindowNext,
    WindowPrevious,
    First,
    Last,
    HalfPageUp,
    HalfPageDown,
    NextTab,
    PreviousTab,
    Search,
    SearchNext,
    SearchPrevious,
    Insert,
    Ex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedResult {
    Pass,
    Pending,
    Cancelled,
    Command(NormalCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prefix {
    Window,
    G,
}

#[derive(Debug, Default)]
pub struct NeovimInput {
    prefix: Option<Prefix>,
}

impl NeovimInput {
    pub fn feed(&mut self, key: KeyEvent) -> FeedResult {
        if let Some(prefix) = self.prefix.take() {
            if key.code == KeyCode::Esc {
                return FeedResult::Cancelled;
            }
            return match prefix {
                Prefix::Window => window_command(key),
                Prefix::G => g_command(key),
            };
        }

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('w') if control => {
                self.prefix = Some(Prefix::Window);
                FeedResult::Pending
            }
            KeyCode::Char('g') if !control => {
                self.prefix = Some(Prefix::G);
                FeedResult::Pending
            }
            KeyCode::Char('h') if !control => command(NormalCommand::Motion(Direction::Left)),
            KeyCode::Char('j') if !control => command(NormalCommand::Motion(Direction::Down)),
            KeyCode::Char('k') if !control => command(NormalCommand::Motion(Direction::Up)),
            KeyCode::Char('l') if !control => command(NormalCommand::Motion(Direction::Right)),
            KeyCode::Char('G') if !control => command(NormalCommand::Last),
            KeyCode::Char('u') if control => command(NormalCommand::HalfPageUp),
            KeyCode::Char('d') if control => command(NormalCommand::HalfPageDown),
            KeyCode::Char('/') if !control => command(NormalCommand::Search),
            KeyCode::Char('n') if !control => command(NormalCommand::SearchNext),
            KeyCode::Char('N') if !control => command(NormalCommand::SearchPrevious),
            KeyCode::Char('i') if !control => command(NormalCommand::Insert),
            KeyCode::Char(':') if !control => command(NormalCommand::Ex),
            _ => FeedResult::Pass,
        }
    }

    pub fn prefix_label(&self) -> Option<&'static str> {
        match self.prefix {
            Some(Prefix::Window) => Some("CTRL-W"),
            Some(Prefix::G) => Some("g"),
            None => None,
        }
    }

    pub fn cancel(&mut self) {
        self.prefix = None;
    }
}

fn command(command: NormalCommand) -> FeedResult {
    FeedResult::Command(command)
}

fn window_command(key: KeyEvent) -> FeedResult {
    let character = match key.code {
        KeyCode::Char(character) => character.to_ascii_lowercase(),
        _ => return FeedResult::Cancelled,
    };
    match character {
        'h' => command(NormalCommand::Window(Direction::Left)),
        'j' => command(NormalCommand::Window(Direction::Down)),
        'k' => command(NormalCommand::Window(Direction::Up)),
        'l' => command(NormalCommand::Window(Direction::Right)),
        'w' => command(NormalCommand::WindowNext),
        'p' => command(NormalCommand::WindowPrevious),
        _ => FeedResult::Cancelled,
    }
}

fn g_command(key: KeyEvent) -> FeedResult {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return FeedResult::Cancelled;
    }
    match key.code {
        KeyCode::Char('g') => command(NormalCommand::First),
        KeyCode::Char('t') => command(NormalCommand::NextTab),
        KeyCode::Char('T') => command(NormalCommand::PreviousTab),
        _ => FeedResult::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
    }

    fn control(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    #[test]
    fn normal_grammar_matches_vanilla_neovim_meanings() {
        let cases: &[(&str, &[KeyEvent], NormalCommand)] = &[
            (
                "move left",
                &[key('h')],
                NormalCommand::Motion(Direction::Left),
            ),
            (
                "move down",
                &[key('j')],
                NormalCommand::Motion(Direction::Down),
            ),
            ("move up", &[key('k')], NormalCommand::Motion(Direction::Up)),
            (
                "move right",
                &[key('l')],
                NormalCommand::Motion(Direction::Right),
            ),
            ("first line", &[key('g'), key('g')], NormalCommand::First),
            ("last line", &[key('G')], NormalCommand::Last),
            ("half page up", &[control('u')], NormalCommand::HalfPageUp),
            (
                "half page down",
                &[control('d')],
                NormalCommand::HalfPageDown,
            ),
            ("next tab", &[key('g'), key('t')], NormalCommand::NextTab),
            (
                "previous tab",
                &[key('g'), key('T')],
                NormalCommand::PreviousTab,
            ),
            ("search", &[key('/')], NormalCommand::Search),
            ("next search match", &[key('n')], NormalCommand::SearchNext),
            (
                "previous search match",
                &[key('N')],
                NormalCommand::SearchPrevious,
            ),
            ("insert", &[key('i')], NormalCommand::Insert),
            ("Ex command", &[key(':')], NormalCommand::Ex),
            (
                "left window",
                &[control('w'), key('h')],
                NormalCommand::Window(Direction::Left),
            ),
            (
                "next window",
                &[control('w'), control('w')],
                NormalCommand::WindowNext,
            ),
            (
                "previous window",
                &[control('w'), key('p')],
                NormalCommand::WindowPrevious,
            ),
        ];

        for (meaning, sequence, expected) in cases {
            let mut input = NeovimInput::default();
            let mut result = FeedResult::Pass;
            for event in *sequence {
                result = input.feed(*event);
            }
            assert_eq!(result, FeedResult::Command(*expected), "{meaning}");
        }
    }

    #[test]
    fn window_suffix_accepts_control_modifier() {
        let mut input = NeovimInput::default();
        assert_eq!(input.feed(control('w')), FeedResult::Pending);
        assert_eq!(
            input.feed(control('l')),
            FeedResult::Command(NormalCommand::Window(Direction::Right))
        );
    }

    #[test]
    fn escape_and_invalid_suffix_cancel_a_prefix() {
        let mut input = NeovimInput::default();
        assert_eq!(input.feed(key('g')), FeedResult::Pending);
        assert_eq!(input.prefix_label(), Some("g"));
        assert_eq!(
            input.feed(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            FeedResult::Cancelled
        );
        assert_eq!(input.prefix_label(), None);

        assert_eq!(input.feed(control('w')), FeedResult::Pending);
        assert_eq!(input.feed(key('x')), FeedResult::Cancelled);
        assert_eq!(input.prefix_label(), None);
    }
}
