struct BuiltinPersonaDef {
    name: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
    system_prompt: &'static str,
}

const BUILTIN_PERSONAS: &[BuiltinPersonaDef] = &[
    BuiltinPersonaDef {
        name: "ozma",
        aliases: &["oz"],
        description: "General-purpose",
        system_prompt: "\
You are Ozma, an assistant running in a terminal.\n\
You are direct, concise, and technically precise.",
    },
    BuiltinPersonaDef {
        name: "ozcode",
        aliases: &[],
        description: "Code writing",
        system_prompt: "\
You are OzCode, a coding assistant running in a terminal.\n\
You write clean, working code. Prefer concrete implementations over theoretical discussion.\n\
Show code first, explain after. Fix bugs with minimal changes.\n\
Match the project's existing style and conventions.",
    },
    BuiltinPersonaDef {
        name: "ozreview",
        aliases: &[],
        description: "Code review",
        system_prompt: "\
You are OzReview, a code review assistant running in a terminal.\n\
Analyze code critically: find bugs, edge cases, security issues, and performance problems.\n\
Be specific — cite line numbers and propose fixes.\n\
Flag what matters, skip what doesn't. Do not rewrite code unless asked.",
    },
    BuiltinPersonaDef {
        name: "ozplan",
        aliases: &[],
        description: "Planning & architecture",
        system_prompt: "\
You are OzPlan, a planning and architecture assistant running in a terminal.\n\
Help break down features into concrete steps. Evaluate tradeoffs explicitly.\n\
Identify risks and dependencies. Produce actionable plans, not abstract advice.\n\
Ask clarifying questions before proposing solutions.",
    },
];

/// Personas shape the system prompt and assistant behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Persona {
    name: String,
    description: String,
    system_prompt: String,
}

impl Persona {
    /// Build a persona.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            system_prompt: system_prompt.into(),
        }
    }

    fn from_builtin_def(def: &BuiltinPersonaDef) -> Self {
        Self::new(def.name, def.description, def.system_prompt)
    }

    /// Build a custom persona.
    pub fn custom(
        name: impl Into<String>,
        description: impl Into<String>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self::new(name, description, system_prompt)
    }

    /// Built-in general-purpose persona.
    pub fn ozma() -> Self {
        Self::from_builtin_def(&BUILTIN_PERSONAS[0])
    }

    /// Built-in coding persona.
    pub fn ozcode() -> Self {
        Self::from_builtin_def(&BUILTIN_PERSONAS[1])
    }

    /// Built-in code review persona.
    pub fn ozreview() -> Self {
        Self::from_builtin_def(&BUILTIN_PERSONAS[2])
    }

    /// Built-in planning persona.
    pub fn ozplan() -> Self {
        Self::from_builtin_def(&BUILTIN_PERSONAS[3])
    }

    /// All built-in personas.
    pub fn builtins() -> Vec<Persona> {
        vec![
            Persona::ozma(),
            Persona::ozcode(),
            Persona::ozreview(),
            Persona::ozplan(),
        ]
    }

    /// Short name used in config, status bar, and database.
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Human-readable description for the picker.
    pub fn description(&self) -> &str {
        self.description.as_str()
    }

    /// The system prompt for this persona.
    pub fn system_prompt(&self) -> &str {
        self.system_prompt.as_str()
    }

    /// Parse a built-in persona name (case-insensitive).
    pub fn from_builtin_name(name: &str) -> Option<Self> {
        BUILTIN_PERSONAS
            .iter()
            .find(|def| {
                def.name.eq_ignore_ascii_case(name)
                    || def
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(name))
            })
            .map(Self::from_builtin_def)
    }

    /// Parse a persona name (case-insensitive). Returns `Ozma` for unknown names.
    pub fn from_name(name: &str) -> Self {
        Self::from_builtin_name(name).unwrap_or_default()
    }

    /// Case-insensitive name match.
    pub fn matches_name(&self, name: &str) -> bool {
        self.name().eq_ignore_ascii_case(name)
            || self.name().eq_ignore_ascii_case("ozma") && name.eq_ignore_ascii_case("oz")
    }
}

impl Default for Persona {
    fn default() -> Self {
        Persona::ozma()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_round_trip() {
        for persona in Persona::builtins() {
            assert_eq!(Persona::from_name(persona.name()), persona.clone());
        }
    }

    #[test]
    fn from_name_case_insensitive_and_unknown_defaults() {
        assert_eq!(Persona::from_name("OzCode"), Persona::ozcode());
        assert_eq!(Persona::from_name("ozcode"), Persona::ozcode());
        assert_eq!(Persona::from_name("unknown"), Persona::ozma());
        assert_eq!(Persona::default(), Persona::ozma());
    }

    #[test]
    fn all_personas_have_prompts() {
        assert!(!Persona::builtins().is_empty());
        for persona in Persona::builtins() {
            assert!(!persona.system_prompt().is_empty());
            assert!(!persona.description().is_empty());
        }
    }

    #[test]
    fn custom_persona_keeps_fields() {
        let p = Persona::custom("ops", "Operations", "You are an SRE assistant.");
        assert_eq!(p.name(), "ops");
        assert_eq!(p.description(), "Operations");
        assert_eq!(p.system_prompt(), "You are an SRE assistant.");
        assert!(p.matches_name("OPS"));
    }
}
