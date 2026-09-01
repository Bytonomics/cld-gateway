// Package tui implements the interactive vendor-picker login menu shown by
// `cld-gateway login`. Ported from crates/gatewayd/src/tui_login.rs:16-65.
package tui

import (
	"errors"
	"fmt"
	"strings"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// Vendor identifies which login flow the user picked from the menu.
// Mirrors crates/gatewayd/src/login/openai.rs LoginSelection.
type Vendor int

const (
	VendorChatGPT Vendor = iota
	VendorAPIKey
)

var errLoginAborted = errors.New("login aborted")

var (
	cyanStyle       = lipgloss.NewStyle().Foreground(lipgloss.Color("6"))
	cyanBoldStyle   = cyanStyle.Bold(true)
	boldStyle       = lipgloss.NewStyle().Bold(true)
	grayStyle       = lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
	selectedStyle   = lipgloss.NewStyle().Foreground(lipgloss.Color("0")).Background(lipgloss.Color("6")).Bold(true)
	borderStyle     = lipgloss.NewStyle().Border(lipgloss.NormalBorder()).Padding(1, 2)
	menuItemLabels  = [2]string{"1. Sign in with ChatGPT", "2. Provide your own API key"}
	menuItemVendors = [2]Vendor{VendorChatGPT, VendorAPIKey}
)

type loginModel struct {
	cursor   int
	chosen   bool
	selected Vendor
	quitting bool
}

func (m loginModel) Init() tea.Cmd {
	return nil
}

func (m loginModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	keyMsg, ok := msg.(tea.KeyMsg)
	if !ok {
		return m, nil
	}

	switch keyMsg.String() {
	case "q", "esc":
		m.quitting = true
		return m, tea.Quit
	case "up":
		if m.cursor > 0 {
			m.cursor--
		}
		return m, nil
	case "down":
		if m.cursor < len(menuItemVendors)-1 {
			m.cursor++
		}
		return m, nil
	case "1":
		m.chosen = true
		m.selected = VendorChatGPT
		return m, tea.Quit
	case "2":
		m.chosen = true
		m.selected = VendorAPIKey
		return m, tea.Quit
	case "enter":
		m.chosen = true
		m.selected = menuItemVendors[m.cursor]
		return m, tea.Quit
	default:
		return m, nil
	}
}

func (m loginModel) View() string {
	var b strings.Builder

	b.WriteString(logoBlock())
	b.WriteString("\n")

	title := boldStyle.Render("Welcome to Gateway") + "\n" +
		grayStyle.Render("Anthropic-compatible proxy for Claude Code → OpenAI") + "\n\n" +
		"Sign in with ChatGPT to use your paid plan, or provide an API key."
	b.WriteString(borderStyle.Render(title))
	b.WriteString("\n\n")

	for idx, label := range menuItemLabels {
		prefix := "  "
		line := label
		if idx == m.cursor {
			prefix = "> "
			line = selectedStyle.Render(prefix + label)
		} else {
			line = prefix + line
		}
		b.WriteString(line)
		b.WriteString("\n")
	}
	b.WriteString("\n")
	b.WriteString(grayStyle.Render("Use ↑/↓ to move, Enter to select, q to quit"))
	b.WriteString("\n")

	return b.String()
}

func logoBlock() string {
	lines := []string{
		"",
		cyanStyle.Render("●──╮                         ╭──●"),
		cyanStyle.Render("●──┼──╮                 ╭────┼──●"),
		cyanStyle.Render("●──┼──┼──────╭──────────┤    │"),
		cyanBoldStyle.Render("●──┼──╯      │  GATEWAY  ├────┼──●"),
		cyanStyle.Render("●──╯         ╰──────────╯"),
		"",
	}
	return strings.Join(lines, "\n")
}

// RunLoginSelector renders the interactive vendor picker (arrow-key
// navigation, numeric shortcuts 1/2, q/Esc to cancel, Enter to confirm) and
// returns the vendor the user picked, matching the interaction parity of
// crates/gatewayd/src/tui_login.rs:16-65.
func RunLoginSelector() (Vendor, error) {
	p := tea.NewProgram(loginModel{})
	finalModel, err := p.Run()
	if err != nil {
		return 0, fmt.Errorf("run login selector: %w", err)
	}

	m, ok := finalModel.(loginModel)
	if !ok || !m.chosen || m.quitting {
		return 0, errLoginAborted
	}

	return m.selected, nil
}
