//! The keyboard shortcuts guide, over everything: Ctrl+/ (F1 too) or
//! "Keyboard shortcuts" in the account menu opens it. Typing filters it,
//! by the keys or by what they do, in either language.

use crate::app::App;
use crate::i18n::{is_pt, tr};
use crate::ui::{dim, rounded_input, truncate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A section of the guide: its name in English and in Portuguese, and its
/// shortcuts as (keys, what they do in English, in Portuguese).
struct Group {
    en: &'static str,
    pt: &'static str,
    keys: &'static [(&'static str, &'static str, &'static str)],
}

/// Every key slick knows, the one list the guide shows.
const GROUPS: &[Group] = &[
    Group {
        en: "General",
        pt: "Geral",
        keys: &[
            ("Ctrl+/ · F1", "This guide", "Este guia de atalhos"),
            ("Ctrl+K · Ctrl+T", "Search conversations", "Procurar conversas"),
            ("Ctrl+N", "New message or channel", "Nova mensagem ou canal"),
            ("Ctrl+P", "Account menu: status, profile, settings", "Menu da conta: status, perfil, configurações"),
            ("Ctrl+Shift+Y", "Set your status", "Definir seu status"),
            ("Ctrl+,", "slick's settings", "Configurações do slick"),
            ("Alt+I · Ctrl+Shift+I", "Channel details, or the person's profile", "Detalhes do canal ou perfil da pessoa"),
            ("Alt+↑ · Alt+↓", "Previous / next conversation", "Conversa anterior / próxima"),
            ("Alt+Shift+↑ · ↓", "Previous / next unread conversation", "Conversa não lida anterior / próxima"),
            ("Alt+← · Alt+→", "Back / forward through opened conversations", "Voltar / avançar pelas conversas abertas"),
            ("Shift+Esc", "Mark everything as read", "Marcar tudo como lido"),
            ("Ctrl+Shift+T", "Threads", "Conversas (threads)"),
            ("Ctrl+Shift+\\", "React to the last message", "Reagir à última mensagem"),
            ("Ctrl+.", "Close the right-hand panel", "Fechar o painel da direita"),
            ("Tab", "Focus: sidebar → message box → thread", "Foco: barra lateral → caixa → conversa"),
            ("PgUp · PgDn", "Scroll the conversation (the wheel too)", "Rolar a conversa (a roda também)"),
            ("Esc", "Close the profile or thread panel", "Fechar o painel de perfil ou de conversa"),
            ("Ctrl+C", "Copy the picked or selected message, else quit", "Copiar a mensagem escolhida ou selecionada, ou sair"),
            ("Ctrl+R", "Reload channels and people", "Recarregar canais e pessoas"),
            ("Ctrl+L", "Sign in again", "Entrar de novo"),
            ("Ctrl+O", "Restart after an update", "Reiniciar após uma atualização"),
        ],
    },
    Group {
        en: "Workspaces",
        pt: "Workspaces",
        keys: &[
            ("Ctrl+1 … Ctrl+9", "Switch to that workspace", "Ir para aquele workspace"),
            ("Ctrl+Tab · Ctrl+PgDn", "Next workspace", "Próximo workspace"),
            ("Ctrl+Shift+Tab", "Previous workspace", "Workspace anterior"),
            ("Ctrl+PgUp", "Previous workspace", "Workspace anterior"),
            ("↑↓ · Enter · Esc", "Workspace list: move, switch, close", "Lista de workspaces: mover, trocar, fechar"),
        ],
    },
    Group {
        en: "Sidebar",
        pt: "Barra lateral",
        keys: &[
            ("↑↓ · j k", "Move", "Mover"),
            ("Home · g", "First row", "Primeira linha"),
            ("End · G", "Last row", "Última linha"),
            ("Enter · → · l · Space", "Open, or fold / unfold a section", "Abrir, ou recolher / expandir a seção"),
            ("/", "Search conversations", "Procurar conversas"),
            ("Shift+J · Shift+K", "On a section header: move it down / up", "Num cabeçalho de seção: mover para baixo / cima"),
            ("Alt+↑ · Alt+↓", "On a section header: move it up / down", "Num cabeçalho de seção: mover para cima / baixo"),
            ("m · Menu", "Context menu (right click too)", "Menu de contexto (clique direito também)"),
            ("q", "Quit", "Sair"),
        ],
    },
    Group {
        en: "Sidebar search",
        pt: "Busca na barra lateral",
        keys: &[
            ("abc…", "Type to filter the conversations", "Digite para filtrar as conversas"),
            ("↑↓", "Move", "Mover"),
            ("Enter", "Open", "Abrir"),
            ("Esc", "Close the search", "Fechar a busca"),
        ],
    },
    Group {
        en: "Message box",
        pt: "Caixa de mensagem",
        keys: &[
            ("Enter", "Send", "Enviar"),
            ("Shift+Enter · Alt+Enter", "New line", "Nova linha"),
            ("Ctrl+J", "New line", "Nova linha"),
            ("Ctrl+U · Ctrl+D", "Clear the box", "Limpar a caixa"),
            ("Ctrl+W · Ctrl+Backspace", "Delete a word", "Apagar uma palavra"),
            ("Ctrl+← · Ctrl+→", "Move by word", "Mover por palavra"),
            ("Ctrl+Delete", "Delete the next word", "Apagar a próxima palavra"),
            ("Ctrl+V", "Paste, pictures included", "Colar, imagens também"),
            ("Ctrl+End · End", "Jump to the latest (End only in an empty box)", "Ir às mais recentes (End só com a caixa vazia)"),
            ("↑", "Empty box: pick a message", "Caixa vazia: escolher uma mensagem"),
            ("Backspace", "Empty box: remove the last attachment", "Caixa vazia: remover o último anexo"),
            ("Esc", "Back to the sidebar", "Voltar à barra lateral"),
            (":", "Emoji autocomplete", "Autocompletar emoji"),
            ("@", "Mention someone", "Mencionar alguém"),
            ("#", "Mention a channel", "Mencionar um canal"),
            ("/", "Slash commands", "Comandos com barra"),
            ("Enter · Tab", "In an autocomplete: pick", "Num autocompletar: escolher"),
            ("u", "Archived channel: unarchive it", "Canal arquivado: desarquivar"),
        ],
    },
    Group {
        en: "Picked message",
        pt: "Mensagem escolhida",
        keys: &[
            ("↑↓", "Pick the one above / below", "Escolher a de cima / de baixo"),
            ("Esc", "Unpick", "Desfazer a escolha"),
            ("R", "React", "Reagir"),
            ("T", "Reply in thread", "Responder na conversa"),
            ("F", "Forward", "Encaminhar"),
            ("A", "Save for later / unsave", "Salvar para depois / tirar dos salvos"),
            ("E", "Edit", "Editar"),
            ("U", "Mark unread from here", "Marcar como não lida a partir daqui"),
            ("P", "Pin / unpin", "Fixar / desafixar"),
            ("L", "Copy the link", "Copiar o link"),
            ("Del", "Delete", "Apagar"),
            ("m · Menu", "Message menu (right click too)", "Menu da mensagem (clique direito também)"),
            ("Ctrl+C", "Copy the message", "Copiar a mensagem"),
            ("Enter · Esc", "While editing: save / cancel", "Editando: salvar / cancelar"),
        ],
    },
    Group {
        en: "Formatting",
        pt: "Formatação",
        keys: &[
            ("Ctrl+B", "Bold", "Negrito"),
            ("Ctrl+I", "Italic", "Itálico"),
            ("Ctrl+Shift+X", "Strikethrough", "Riscado"),
            ("Ctrl+Shift+C", "Inline code", "Código"),
            ("Ctrl+Alt+Shift+C", "Code block", "Bloco de código"),
            ("Ctrl+Shift+9", "Quote", "Citação"),
            ("Ctrl+Shift+8", "Bulleted list", "Lista com marcadores"),
            ("Ctrl+Shift+7", "Numbered list", "Lista numerada"),
            ("Alt+B · Alt+X · Alt+C", "Bold, strike, code where Ctrl+Shift can't", "Negrito, riscado, código onde Ctrl+Shift não vai"),
        ],
    },
    Group {
        en: "Thread panel",
        pt: "Painel da conversa",
        keys: &[
            ("Ctrl+B", "Also send to the channel", "Enviar também para o canal"),
            ("Esc", "Back to the message box", "Voltar à caixa de mensagem"),
            ("PgUp · PgDn", "Scroll the thread", "Rolar a conversa"),
        ],
    },
    Group {
        en: "Threads view",
        pt: "Tela de conversas",
        keys: &[
            ("↑↓ · j k", "Move", "Mover"),
            ("Home · g", "First thread", "Primeira conversa"),
            ("End · G", "Last thread", "Última conversa"),
            ("PgUp · PgDn", "Scroll", "Rolar"),
            ("Enter · → · l", "Open the thread", "Abrir a conversa"),
            ("r · i", "Reply right here", "Responder aqui mesmo"),
            ("e · Space", "Expand", "Expandir"),
            ("Esc", "Back to the sidebar", "Voltar à barra lateral"),
        ],
    },
    Group {
        en: "Pins",
        pt: "Fixados",
        keys: &[
            ("Alt+1 · Alt+2", "Messages / Pins tab", "Aba Mensagens / Fixados"),
            ("↑↓ · PgUp · PgDn", "Scroll the pins", "Rolar os fixados"),
            ("Esc", "Back to the messages", "Voltar às mensagens"),
        ],
    },
    Group {
        en: "Image viewer",
        pt: "Visualizador de imagens",
        keys: &[
            ("o", "Open in another app", "Abrir em outro aplicativo"),
            ("d", "Download", "Baixar"),
            ("v · b", "Open its link", "Abrir o link"),
            ("Esc", "Close (any other key too)", "Fechar (qualquer outra tecla também)"),
        ],
    },
    Group {
        en: "Menus and dialogs",
        pt: "Menus e diálogos",
        keys: &[
            ("↑↓ · j k", "Move", "Mover"),
            ("Enter · → · l · Space", "Choose", "Escolher"),
            ("Esc · ← · h", "Back / close", "Voltar / fechar"),
            ("Tab · Shift+Tab", "Next / previous field", "Próximo campo / anterior"),
            ("Ctrl+Enter", "Save or send", "Salvar ou enviar"),
            ("abc… · ↑↓ · Enter", "Reaction picker: search, move, pick", "Seletor de reação: buscar, mover, escolher"),
        ],
    },
    Group {
        en: "Channel details",
        pt: "Detalhes do canal",
        keys: &[
            ("Alt+1 · Alt+2 · Alt+3", "About, Members, Settings tabs", "Abas Sobre, Membros, Configurações"),
            ("Tab · Shift+Tab · ← →", "Next / previous tab", "Próxima aba / anterior"),
            ("Alt+S", "Star / unstar", "Favoritar / desfavoritar"),
            ("Alt+N", "Notifications", "Notificações"),
            ("abc…", "Members tab: type to filter", "Aba Membros: digite para filtrar"),
            ("Esc", "Close", "Fechar"),
        ],
    },
];

/// The guide, open.
#[derive(Default)]
pub struct Help {
    query: String,
    /// The first line shown.
    scroll: usize,
    /// From the last draw: the furthest scroll, the rows shown and the frame.
    max: usize,
    page: usize,
    rect: Rect,
}

/// Ctrl+/ as each terminal sends it: `/` with the kitty protocol, else
/// 0x1F, which crossterm reads as Ctrl+7 or Ctrl+_. F1 too. Ctrl+Shift+7
/// stays the numbered list.
pub fn is_toggle(k: &KeyEvent) -> bool {
    let m = k.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::F(1) => true,
        KeyCode::Char('/' | '_') => ctrl,
        KeyCode::Char('7') => ctrl && !m.contains(KeyModifiers::SHIFT),
        _ => false,
    }
}

/// For matching: lower case and without accents, one char for one char so
/// positions still point into the text.
fn fold(c: char) -> char {
    match c.to_lowercase().next().unwrap_or(c) {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        c => c,
    }
}

/// A match: where it starts and ends, in chars.
type Found = Option<(usize, usize)>;

/// Where the folded `needle` is in `text`, in chars.
fn find(text: &str, needle: &[char]) -> Found {
    if needle.is_empty() {
        return None;
    }
    let hay: Vec<char> = text.chars().map(fold).collect();
    hay.windows(needle.len()).position(|w| w == needle).map(|i| (i, i + needle.len()))
}

/// A shortcut the query leaves, with where the query is in its keys and words.
struct Hit {
    keys: &'static str,
    what: &'static str,
    in_keys: Found,
    in_what: Found,
}

/// The groups with something left, in the current language: a group whose
/// name matches keeps all of its shortcuts. The other language's words
/// match too, so "negrito" finds Bold.
fn filtered(query: &str) -> Vec<(&'static str, Found, Vec<Hit>)> {
    let q: Vec<char> = query.trim().chars().map(fold).collect();
    let pt = is_pt();
    GROUPS
        .iter()
        .filter_map(|g| {
            let (name, other) = if pt { (g.pt, g.en) } else { (g.en, g.pt) };
            let in_name = find(name, &q);
            let all = q.is_empty() || in_name.is_some() || find(other, &q).is_some();
            let hits: Vec<Hit> = g
                .keys
                .iter()
                .filter_map(|&(keys, en, pt_what)| {
                    let (what, other) = if pt { (pt_what, en) } else { (en, pt_what) };
                    let (in_keys, in_what) = (find(keys, &q), find(what, &q));
                    let hit = all || in_keys.is_some() || in_what.is_some() || find(other, &q).is_some();
                    hit.then_some(Hit { keys, what, in_keys, in_what })
                })
                .collect();
            (!hits.is_empty()).then_some((name, in_name, hits))
        })
        .collect()
}

/// `text` in `style` with the chars at `hit` marked, cut to `w` cells;
/// also the cells it took.
fn marked(text: &str, hit: Found, style: Style, w: usize) -> (Vec<Span<'static>>, usize) {
    let mark = style.fg(Color::Black).bg(Color::Yellow);
    let cut = text.width() > w;
    let room = if cut { w.saturating_sub(1) } else { w };
    let (mut spans, mut run, mut lit, mut used) = (Vec::new(), String::new(), false, 0);
    for (i, ch) in text.chars().enumerate() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > room {
            break;
        }
        used += cw;
        let on = hit.is_some_and(|(a, b)| (a..b).contains(&i));
        if on != lit && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), if lit { mark } else { style }));
        }
        lit = on;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if lit { mark } else { style }));
    }
    if cut && w > 0 {
        spans.push(Span::styled("…", style));
        used += 1;
    }
    (spans, used)
}

impl App {
    /// Ctrl+/: opens the guide, or closes it.
    pub(crate) fn toggle_help(&mut self) {
        if self.help.take().is_none() {
            self.open_help();
        }
        self.dirty = true;
    }

    pub(crate) fn open_help(&mut self) {
        (self.menu, self.account.menu) = (None, None);
        self.help = Some(Help::default());
        self.dirty = true;
    }

    /// Letters search, so only the arrows and the page keys scroll.
    pub(crate) fn help_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        if is_toggle(&k) {
            self.help = None;
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let Some(h) = self.help.as_mut() else { return };
        let page = h.page.max(1);
        match k.code {
            KeyCode::Esc if !h.query.is_empty() => (h.query, h.scroll) = (String::new(), 0),
            KeyCode::Esc => self.help = None,
            KeyCode::Up => h.scroll = h.scroll.saturating_sub(1),
            KeyCode::Down => h.scroll = (h.scroll + 1).min(h.max),
            KeyCode::PageUp => h.scroll = h.scroll.saturating_sub(page),
            KeyCode::PageDown => h.scroll = (h.scroll + page).min(h.max),
            KeyCode::Home => h.scroll = 0,
            KeyCode::End => h.scroll = h.max,
            KeyCode::Backspace => {
                h.query.pop();
                h.scroll = 0;
            }
            KeyCode::Char('u') if ctrl => (h.query, h.scroll) = (String::new(), 0),
            KeyCode::Char(c) if !ctrl && !alt => {
                h.query.push(c);
                h.scroll = 0;
            }
            _ => {}
        }
    }

    pub(crate) fn help_paste(&mut self, s: &str) {
        if let Some(h) = self.help.as_mut() {
            h.query.push_str(s.lines().next().unwrap_or(""));
            h.scroll = 0;
        }
    }

    /// The wheel scrolls; a click outside closes.
    pub(crate) fn help_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let Some(h) = self.help.as_mut() else { return };
        match m.kind {
            MouseEventKind::ScrollUp => h.scroll = h.scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => h.scroll = (h.scroll + 3).min(h.max),
            MouseEventKind::Down(_) if !h.rect.contains(at) => self.help = None,
            _ => return,
        }
        self.dirty = true;
    }
}

/// The guide, centered: the search field, the groups and a hint.
pub fn draw(f: &mut Frame, app: &mut App) {
    let Some(h) = app.help.as_ref() else { return };
    let query = h.query.clone();
    let area = f.area();
    let w = area.width.saturating_sub(4).clamp(area.width.min(30), 84);
    let ht = area.height.saturating_sub(2).clamp(area.height.min(12), 40);
    let rect = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - ht) / 2, w, ht);
    f.render_widget(Clear, rect);
    let title = Line::from(tr(" Keyboard shortcuts ", " Atalhos de teclado ")).bold();
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(dim()).title(title);
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    if inner.height < 6 || inner.width < 10 {
        if let Some(h) = app.help.as_mut() {
            h.rect = rect;
        }
        return;
    }

    // The search field, always focused.
    let field = Rect::new(inner.x, inner.y, inner.width, 3);
    let text = if query.is_empty() {
        Line::from(tr("Search keys or what they do", "Buscar teclas ou o que elas fazem")).style(dim())
    } else {
        Line::from(query.clone())
    };
    let text_area = rounded_input(f, field, true, text);
    let cx = text_area.x + query.width().min(text_area.width.saturating_sub(1) as usize) as u16;
    f.set_cursor_position((cx, text_area.y));

    // The list between the field and the hint, a blank row above the hint.
    let list = Rect::new(inner.x, field.bottom(), inner.width, inner.height.saturating_sub(5));
    let groups = filtered(&query);
    let count: usize = groups.iter().map(|(_, _, hits)| hits.len()).sum();
    let lw = list.width as usize;
    let key_w = GROUPS.iter().flat_map(|g| g.keys).map(|(k, _, _)| k.width()).max().unwrap_or(0).min(lw / 2) + 2;
    let what_w = lw.saturating_sub(key_w + 3);
    let mut lines: Vec<Line> = Vec::new();
    for (n, (name, in_name, hits)) in groups.iter().enumerate() {
        if n > 0 {
            lines.push(Line::from(""));
        }
        let mut head = vec![Span::raw(" ")];
        head.extend(marked(name, *in_name, Style::new().bold(), lw.saturating_sub(1)).0);
        lines.push(Line::from(head));
        for hit in hits {
            let (keys, used) = marked(hit.keys, hit.in_keys, Style::new().fg(Color::Cyan), key_w - 2);
            let mut row = vec![Span::raw("   ")];
            row.extend(keys);
            row.push(Span::raw(" ".repeat(key_w.saturating_sub(used))));
            row.extend(marked(hit.what, hit.in_what, Style::new(), what_w).0);
            lines.push(Line::from(row));
        }
    }
    if groups.is_empty() {
        lines.push(Line::from(format!(" {}", tr("No shortcuts match", "Nenhum atalho encontrado"))).style(dim()));
    }
    let page = list.height as usize;
    let max = lines.len().saturating_sub(page);
    let scroll = app.help.as_ref().map_or(0, |h| h.scroll).min(max);
    let shown: Vec<Line> = lines.into_iter().skip(scroll).take(page).collect();
    f.render_widget(Paragraph::new(shown), list);
    // More above or below.
    if scroll > 0 {
        f.render_widget(Paragraph::new("↑").style(dim()), Rect::new(list.right().saturating_sub(1), list.y, 1, 1));
    }
    if scroll < max && list.height > 0 {
        f.render_widget(Paragraph::new("↓").style(dim()), Rect::new(list.right().saturating_sub(1), list.bottom() - 1, 1, 1));
    }

    // The hint, and how many shortcuts are shown on the right.
    let by = inner.bottom().saturating_sub(1);
    let n = match count {
        1 => tr("1 shortcut", "1 atalho").to_string(),
        _ => trf!("{count} shortcuts", "{count} atalhos"),
    };
    let nw = (n.width() as u16 + 1).min(inner.width);
    let hint_w = inner.width - nw;
    let hint = tr("type to search · ↑↓ PgUp PgDn scroll · Esc closes", "digite para buscar · ↑↓ PgUp PgDn rolam · Esc fecha");
    f.render_widget(Paragraph::new(format!(" {}", truncate(hint, hint_w.saturating_sub(2) as usize))).style(dim()), Rect::new(inner.x, by, hint_w, 1));
    f.render_widget(Paragraph::new(n).style(dim()), Rect::new(inner.x + hint_w, by, nw, 1));

    if let Some(h) = app.help.as_mut() {
        (h.max, h.page, h.scroll, h.rect) = (max, page, scroll, rect);
    }
}
