/// Predefined personas that shape the system prompt and assistant behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Persona {
    /// General-purpose assistant.
    Ozma,
    /// Coding-focused assistant.
    OzCode,
    /// Code review assistant.
    OzReview,
    /// Architecture and planning assistant.
    OzPlan,
}

impl Persona {
    /// Short name used in config, status bar, and database.
    pub fn name(&self) -> &'static str {
        match self {
            Persona::Ozma => "ozma",
            Persona::OzCode => "ozcode",
            Persona::OzReview => "ozreview",
            Persona::OzPlan => "ozplan",
        }
    }

    /// Human-readable description for the picker.
    pub fn description(&self) -> &'static str {
        match self {
            Persona::Ozma => "General-purpose",
            Persona::OzCode => "Code writing",
            Persona::OzReview => "Code review",
            Persona::OzPlan => "Planning & architecture",
        }
    }

    /// The system prompt for this persona.
    pub fn system_prompt(&self) -> &'static str {
        match self {
            Persona::Ozma => {
                "\
You are Ozma, an assistant running in a terminal.\n\
You are direct, concise, and technically precise."
            }
            Persona::OzCode => {
                "\
You are OzCode, a coding assistant running in a terminal.\n\
You write clean, working code. Prefer concrete implementations over theoretical discussion.\n\
Show code first, explain after. Fix bugs with minimal changes.\n\
Match the project's existing style and conventions."
            }
            Persona::OzReview => {
                "\
You are OzReview, a code review assistant running in a terminal.\n\
Analyze code critically: find bugs, edge cases, security issues, and performance problems.\n\
Be specific — cite line numbers and propose fixes.\n\
Flag what matters, skip what doesn't. Do not rewrite code unless asked."
            }
            Persona::OzPlan => {
                "\
You are OzPlan, a planning and architecture assistant running in a terminal.\n\
Help break down features into concrete steps. Evaluate tradeoffs explicitly.\n\
Identify risks and dependencies. Produce actionable plans, not abstract advice.\n\
Ask clarifying questions before proposing solutions."
            }
        }
    }

    /// All available personas.
    pub fn all() -> &'static [Persona] {
        &[
            Persona::Ozma,
            Persona::OzCode,
            Persona::OzReview,
            Persona::OzPlan,
        ]
    }

    /// Parse a persona name (case-insensitive). Returns `Ozma` for unknown names.
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "ozma" | "oz" => Persona::Ozma,
            "ozcode" => Persona::OzCode,
            "ozreview" => Persona::OzReview,
            "ozplan" => Persona::OzPlan,
            _ => Persona::Ozma,
        }
    }
}

impl Default for Persona {
    fn default() -> Self {
        Persona::Ozma
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_round_trip() {
        for persona in Persona::all() {
            assert_eq!(Persona::from_name(persona.name()), *persona);
        }
    }

    #[test]
    fn from_name_case_insensitive_and_unknown_defaults() {
        assert_eq!(Persona::from_name("OzCode"), Persona::OzCode);
        assert_eq!(Persona::from_name("ozcode"), Persona::OzCode);
        assert_eq!(Persona::from_name("unknown"), Persona::Ozma);
        assert_eq!(Persona::default(), Persona::Ozma);
    }

    #[test]
    fn all_personas_have_prompts() {
        assert!(!Persona::all().is_empty());
        for persona in Persona::all() {
            assert!(!persona.system_prompt().is_empty());
            assert!(!persona.description().is_empty());
        }
    }
}
