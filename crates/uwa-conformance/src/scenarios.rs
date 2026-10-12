//! The conformance corpus.
//!
//! 30 scenarios across 7 tags. Add new ones at the bottom; never renumber
//! or rename existing ids — the report compares across runs.

use serde_json::json;
use uwa_tools::{ToolDefinition, ToolParseOutcome};

use crate::checks::*;
use crate::scenario::{CheckResult, Scenario, Tag};

// ---------------------------------------------------------------------------
// Tool builders
// ---------------------------------------------------------------------------

fn tools_weather() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "get_weather",
        "Look up the current weather by city.",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string", "description": "City name" } },
            "required": ["city"]
        }),
    )]
}

fn tools_calc() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "calc",
        "Evaluate an arithmetic expression.",
        json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string" },
                "precision": { "type": "integer" }
            },
            "required": ["expression"]
        }),
    )]
}

fn tools_units() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "convert",
        "Convert a quantity between units.",
        json!({
            "type": "object",
            "properties": {
                "value": { "type": "number" },
                "from": { "type": "string", "enum": ["c", "f", "k"] },
                "to":   { "type": "string", "enum": ["c", "f", "k"] }
            },
            "required": ["value", "from", "to"]
        }),
    )]
}

// ---------------------------------------------------------------------------
// Check functions
// ---------------------------------------------------------------------------

fn tools_no_args() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "ping",
        "Ping the server.",
        json!({ "type": "object", "properties": {} }),
    )]
}

fn tools_person() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "create_person",
        "Create a person record.",
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" },
                "active": { "type": "boolean" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "address": {
                    "type": "object",
                    "properties": {
                        "street": { "type": "string" },
                        "city": { "type": "string" },
                        "zip": { "type": "string" }
                    },
                    "required": ["city"]
                }
            },
            "required": ["name"]
        }),
    )]
}

fn tools_multi_parallel() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            "get_weather",
            "Weather by city.",
            json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        ),
        ToolDefinition::new(
            "get_time",
            "Local time by city.",
            json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        ),
    ]
}

fn tools_many() -> Vec<ToolDefinition> {
    let mut out = Vec::new();
    for name in [
        "search_web",
        "read_file",
        "write_file",
        "list_dir",
        "run_shell",
        "http_get",
        "send_email",
        "create_calendar_event",
        "translate",
        "summarize",
        "tokenize",
        "embed_text",
        "query_db",
        "insert_row",
        "delete_row",
        "list_tables",
        "encrypt",
        "decrypt",
        "hash_sha256",
        "random_bytes",
    ] {
        out.push(ToolDefinition::new(
            name,
            format!("Tool `{name}` — see docs."),
            json!({
                "type": "object",
                "properties": { "input": { "type": "string" } },
                "required": ["input"]
            }),
        ));
    }
    out
}

fn tools_ambiguous() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            "get_weather",
            "Current weather.",
            json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        ),
        ToolDefinition::new(
            "get_forecast",
            "Weather forecast for the next N days.",
            json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "days": { "type": "integer" }
                },
                "required": ["city", "days"]
            }),
        ),
    ]
}

fn tools_math_or_sql() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            "calc",
            "Evaluate arithmetic.",
            json!({
                "type": "object",
                "properties": { "expression": { "type": "string" } },
                "required": ["expression"]
            }),
        ),
        ToolDefinition::new(
            "run_sql",
            "Run a read-only SQL query.",
            json!({
                "type": "object",
                "properties": { "sql": { "type": "string" } },
                "required": ["sql"]
            }),
        ),
    ]
}

fn tools_unicode() -> Vec<ToolDefinition> {
    tools_weather()
}

fn tools_special_chars() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "echo",
        "Echo the input string back.",
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        }),
    )]
}

fn tools_long() -> Vec<ToolDefinition> {
    tools_special_chars()
}

fn tools_empty_string() -> Vec<ToolDefinition> {
    tools_special_chars()
}

fn tools_nullable() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "set_preference",
        "Set a nullable user preference.",
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string" },
                "value": { "type": ["string", "null"] }
            },
            "required": ["key", "value"]
        }),
    )]
}

fn tools_edge_many() -> Vec<ToolDefinition> {
    tools_many()
}

fn tools_edge_long_desc() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "get_weather",
        "Look up the current weather by city. \
         Returns temperature, humidity, wind speed and direction, \
         UV index, air quality index, sunrise and sunset times, \
         a five-day forecast, and any active weather alerts for the \
         requested location. Accepts city names, IATA airport codes, \
         or latitude,longitude pairs as the `city` argument. If the \
         location is ambiguous, returns the first matching result and \
         includes a `candidates` field listing the other matches.",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    )]
}

fn tools_edge_unusual() -> Vec<ToolDefinition> {
    vec![ToolDefinition::new(
        "convert-v2",
        "Unit converter (dashed tool name).",
        json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": ["value"]
        }),
    )]
}

// ---------------------------------------------------------------------------
// Check functions
// ---------------------------------------------------------------------------

fn check_basic_simple(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "get_weather" {
            return Err(format!("name `{}`", c.name));
        }
        requires(c, &["city"])?;
        if arg_str(c, "city")? != "NYC" {
            return Err(format!("city `{}`", arg_str(c, "city")?));
        }
        Ok(())
    })
}

fn check_basic_plain(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Model should answer without calling any tool.
    if !out.calls.is_empty() {
        return fail(format!(
            "expected plain answer, got {} call(s)",
            out.calls.len()
        ));
    }
    if out.text.trim().is_empty() {
        return fail("expected non-empty plain answer");
    }
    pass()
}

fn check_basic_no_args(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "ping" {
            return Err(format!("name `{}`", c.name));
        }
        if !c.arguments.is_object() {
            return Err("arguments must be an object".into());
        }
        Ok(())
    })
}

fn check_basic_no_tool_needed(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // User asks a general question with no matching tool available.
    if out.calls.is_empty() {
        pass()
    } else {
        fail(format!("expected no calls, got {}", out.calls.len()))
    }
}

fn check_basic_multiturn(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Single-turn approximation: the model should still answer with a call.
    exactly_one(out, |c| {
        if c.name != "get_weather" {
            return Err(format!("name `{}`", c.name));
        }
        requires(c, &["city"])
    })
}

// -- schema --

fn check_schema_required(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Model must include `city` even though the prompt doesn't spell it out.
    exactly_one(out, |c| {
        requires(c, &["city"])?;
        if arg_str(c, "city").is_err() {
            return Err("city not a string".into());
        }
        Ok(())
    })
}

fn check_schema_enum(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "convert" {
            return Err(format!("name `{}`", c.name));
        }
        let from = arg_str(c, "from")?;
        let to = arg_str(c, "to")?;
        if !["c", "f", "k"].contains(&from) {
            return Err(format!("from `{from}` not in enum"));
        }
        if !["c", "f", "k"].contains(&to) {
            return Err(format!("to `{to}` not in enum"));
        }
        Ok(())
    })
}

fn check_schema_integer(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "calc" {
            return Err(format!("name `{}`", c.name));
        }
        requires(c, &["expression"])?;
        // If precision is present, it must be an integer.
        if let Some(p) = c.arguments.get("precision") {
            if !p.is_i64() {
                return Err(format!("precision `{p}` not integer"));
            }
        }
        Ok(())
    })
}

fn check_schema_boolean(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "create_person" {
            return Err(format!("name `{}`", c.name));
        }
        arg_bool(c, "active")?;
        Ok(())
    })
}

fn check_schema_array(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "create_person" {
            return Err(format!("name `{}`", c.name));
        }
        let arr = arg_array(c, "tags")?;
        if arr.is_empty() {
            return Err("tags must have at least one element".into());
        }
        for (i, v) in arr.iter().enumerate() {
            if !v.is_string() {
                return Err(format!("tags[{i}] not a string"));
            }
        }
        Ok(())
    })
}

fn check_schema_nested(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "create_person" {
            return Err(format!("name `{}`", c.name));
        }
        let addr = arg_object(c, "address")?;
        if !addr.get("city").map(|v| v.is_string()).unwrap_or(false) {
            return Err("address.city missing".into());
        }
        Ok(())
    })
}

fn check_schema_deep(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Deeply nested: address is an object, but the model must fill all
    // three sub-keys.
    exactly_one(out, |c| {
        let addr = arg_object(c, "address")?;
        for k in ["street", "city", "zip"] {
            if !addr.contains_key(k) {
                return Err(format!("address.{k} missing"));
            }
        }
        Ok(())
    })
}

fn check_schema_many_props(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Many top-level fields; model must not silently drop them.
    exactly_one(out, |c| {
        if c.name != "create_person" {
            return Err(format!("name `{}`", c.name));
        }
        requires(c, &["name", "age", "active", "tags", "address"])
    })
}

// -- parallel --

fn check_parallel_same(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_n(out, 2, |calls| {
        for c in calls {
            if c.name != "get_weather" {
                return Err(format!("unexpected name `{}`", c.name));
            }
            requires(c, &["city"])?;
        }
        Ok(())
    })
}

fn check_parallel_different(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_n(out, 2, |calls| {
        let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
        if !names.contains(&"get_weather") || !names.contains(&"get_time") {
            return Err(format!("names {names:?}"));
        }
        Ok(())
    })
}

fn check_parallel_three(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_n(out, 3, |calls| {
        for c in calls {
            if c.name != "get_weather" {
                return Err(format!("unexpected `{}`", c.name));
            }
        }
        Ok(())
    })
}

fn check_parallel_one_of_two(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // User asks for one specific piece of info; only one call is needed.
    exactly_one(out, |c| {
        if c.name != "get_time" {
            return Err(format!("expected get_time, got `{}`", c.name));
        }
        Ok(())
    })
}

// -- arguments --

fn check_args_unicode(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        let city = arg_str(c, "city")?;
        if !city.contains("東京") {
            return Err(format!("city `{city}` missing unicode"));
        }
        Ok(())
    })
}

fn check_args_special(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        let t = arg_str(c, "text")?;
        for needle in ["\"", "\\", "\n"] {
            if !t.contains(needle) {
                return Err(format!("text missing {needle:?}"));
            }
        }
        Ok(())
    })
}

fn check_args_long(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        let t = arg_str(c, "text")?;
        if t.len() < 500 {
            return Err(format!("text too short: {} chars", t.len()));
        }
        Ok(())
    })
}

fn check_args_empty(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        let t = arg_str(c, "text")?;
        if !t.is_empty() {
            return Err(format!("expected empty string, got {t:?}"));
        }
        Ok(())
    })
}

fn check_args_null(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        let v = c
            .arguments
            .get("value")
            .ok_or_else(|| "value: missing".to_string())?;
        if !v.is_null() {
            return Err(format!("value must be null, got {v}"));
        }
        Ok(())
    })
}

// -- selection --

fn check_select_correct(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "translate" {
            return Err(format!("expected translate, got `{}`", c.name));
        }
        Ok(())
    })
}

fn check_select_similar(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // "the next 3 days" → forecast, not weather.
    exactly_one(out, |c| {
        if c.name != "get_forecast" {
            return Err(format!("expected get_forecast, got `{}`", c.name));
        }
        Ok(())
    })
}

fn check_select_domain(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // SQL question → run_sql, not calc.
    exactly_one(out, |c| {
        if c.name != "run_sql" {
            return Err(format!("expected run_sql, got `{}`", c.name));
        }
        Ok(())
    })
}

fn check_select_none_match(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Nothing to call; the model should answer directly.
    if out.calls.is_empty() {
        pass()
    } else {
        fail(format!(
            "expected no call (no matching tool), got `{}`",
            out.calls[0].name
        ))
    }
}

// -- errors --

fn check_errors_invalid_json(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // After a malformed first attempt, the pipeline's retry loop kicks in
    // and the model should recover with a valid call. We only see the
    // final outcome; a valid final call is a Pass.
    if out.calls.len() == 1 && out.calls[0].name == "get_weather" {
        pass()
    } else if out.calls.is_empty() {
        fail("no valid call after retry")
    } else {
        fail(format!("unexpected {} calls", out.calls.len()))
    }
}

fn check_errors_unknown_tool(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Model invented a tool; it must not appear in the final output.
    for c in &out.calls {
        if c.name == "delete_everything" {
            return fail("invented tool survived");
        }
    }
    // Either it correctly calls nothing, or calls a real tool.
    pass()
}

fn check_errors_wrong_type(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Model may have put a string where an int belongs; if so, retry
    // should have fixed it. Final args must be int.
    exactly_one(out, |c| {
        if let Some(p) = c.arguments.get("precision") {
            if !p.is_i64() {
                return Err(format!("precision still wrong type: {p}"));
            }
        }
        Ok(())
    })
}

// -- edge --

fn check_edge_empty_tools(out: &ToolParseOutcome, tools: &[ToolDefinition]) -> CheckResult {
    if !tools.is_empty() {
        return fail("scenario tools must be empty");
    }
    if !out.calls.is_empty() {
        return fail("no tools declared, but a call was produced");
    }
    pass()
}

fn check_edge_many_tools(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "translate" {
            return Err(format!("expected translate, got `{}`", c.name));
        }
        requires(c, &["input"])
    })
}

fn check_edge_long_description(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    // Same as basic, but with a verbose tool description.
    check_basic_simple(out, &[])
}

fn check_edge_unusual_name(out: &ToolParseOutcome, _: &[ToolDefinition]) -> CheckResult {
    exactly_one(out, |c| {
        if c.name != "convert-v2" {
            return Err(format!("expected `convert-v2`, got `{}`", c.name));
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

pub static SCENARIOS: &[Scenario] = &[
    // ---------- basic ----------
    Scenario {
        id: "basic.simple_call",
        name: "One tool, one string argument",
        tags: &[Tag::Basic],
        user: "What's the weather in NYC?",
        system: None,
        tools: tools_weather,
        check: check_basic_simple,
    },
    Scenario {
        id: "basic.plain_answer",
        name: "No tool needed — answer directly",
        tags: &[Tag::Basic],
        user: "Say hello in one short sentence.",
        system: None,
        tools: tools_weather,
        check: check_basic_plain,
    },
    Scenario {
        id: "basic.no_args_tool",
        name: "Tool with an empty schema",
        tags: &[Tag::Basic],
        user: "Ping the server.",
        system: None,
        tools: tools_no_args,
        check: check_basic_no_args,
    },
    Scenario {
        id: "basic.no_tool_match",
        name: "No matching tool available",
        tags: &[Tag::Basic],
        user: "Summarize the plot of Hamlet.",
        system: None,
        tools: tools_weather,
        check: check_basic_no_tool_needed,
    },
    Scenario {
        id: "basic.multiturn_shape",
        name: "Assistant turn with a tool call",
        tags: &[Tag::Basic],
        user: "Weather in NYC?",
        system: None,
        tools: tools_weather,
        check: check_basic_multiturn,
    },

    // ---------- schema ----------
    Scenario {
        id: "schema.required_missing",
        name: "Model must include required field",
        tags: &[Tag::Schema],
        user: "Check the weather for the city of Boston.",
        system: None,
        tools: tools_weather,
        check: check_schema_required,
    },
    Scenario {
        id: "schema.enum_value",
        name: "Enum values respected",
        tags: &[Tag::Schema],
        user: "Convert 100 fahrenheit to celsius.",
        system: None,
        tools: tools_units,
        check: check_schema_enum,
    },
    Scenario {
        id: "schema.integer_type",
        name: "Integer typed field",
        tags: &[Tag::Schema],
        user: "Calculate 2+2 with 3 digits of precision.",
        system: None,
        tools: tools_calc,
        check: check_schema_integer,
    },
    Scenario {
        id: "schema.boolean_type",
        name: "Boolean typed field",
        tags: &[Tag::Schema],
        user: "Create a person named Alice, active.",
        system: None,
        tools: tools_person,
        check: check_schema_boolean,
    },
    Scenario {
        id: "schema.array_of_strings",
        name: "Array of strings",
        tags: &[Tag::Schema],
        user: "Create a person named Bob with tags friend and colleague.",
        system: None,
        tools: tools_person,
        check: check_schema_array,
    },
    Scenario {
        id: "schema.nested_object",
        name: "Nested object (one level)",
        tags: &[Tag::Schema],
        user: "Create a person named Carol living in Seattle.",
        system: None,
        tools: tools_person,
        check: check_schema_nested,
    },
    Scenario {
        id: "schema.deep_nested",
        name: "Deeply nested address fields",
        tags: &[Tag::Schema],
        user: "Create a person named Dan at 12 Main St, Portland, OR 97201.",
        system: None,
        tools: tools_person,
        check: check_schema_deep,
    },
    Scenario {
        id: "schema.many_properties",
        name: "All top-level fields populated",
        tags: &[Tag::Schema],
        user: "Create a person named Eve, age 30, active, tagged admin, in Paris.",
        system: None,
        tools: tools_person,
        check: check_schema_many_props,
    },

    // ---------- parallel ----------
    Scenario {
        id: "parallel.two_same",
        name: "Two calls to the same tool",
        tags: &[Tag::Parallel],
        user: "Weather in NYC and LA.",
        system: None,
        tools: tools_weather,
        check: check_parallel_same,
    },
    Scenario {
        id: "parallel.two_different",
        name: "Two different tools in one turn",
        tags: &[Tag::Parallel],
        user: "Weather and local time in Tokyo.",
        system: None,
        tools: tools_multi_parallel,
        check: check_parallel_different,
    },
    Scenario {
        id: "parallel.three",
        name: "Three calls in one turn",
        tags: &[Tag::Parallel],
        user: "Weather in NYC, LA, and Chicago.",
        system: None,
        tools: tools_weather,
        check: check_parallel_three,
    },
    Scenario {
        id: "parallel.one_needed",
        name: "Only one of several tools needed",
        tags: &[Tag::Parallel],
        user: "What time is it in Berlin?",
        system: None,
        tools: tools_multi_parallel,
        check: check_parallel_one_of_two,
    },

    // ---------- arguments ----------
    Scenario {
        id: "args.unicode",
        name: "Unicode argument (CJK)",
        tags: &[Tag::Arguments],
        user: "What's the weather in 東京?",
        system: None,
        tools: tools_unicode,
        check: check_args_unicode,
    },
    Scenario {
        id: "args.special_chars",
        name: "Quotes, backslashes and newlines",
        tags: &[Tag::Arguments],
        user: "Echo this exactly: He said \"hi\\there\"\nand left.",
        system: None,
        tools: tools_special_chars,
        check: check_args_special,
    },
    Scenario {
        id: "args.long_string",
        name: "Long string argument (500+ chars)",
        tags: &[Tag::Arguments],
        user: "Echo the following text exactly: The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.",
        system: None,
        tools: tools_long,
        check: check_args_long,
    },
    Scenario {
        id: "args.empty_string",
        name: "Empty string argument",
        tags: &[Tag::Arguments],
        user: "Echo an empty string.",
        system: None,
        tools: tools_empty_string,
        check: check_args_empty,
    },
    Scenario {
        id: "args.null_value",
        name: "Null in a nullable field",
        tags: &[Tag::Arguments],
        user: "Set the user preference `theme` to null.",
        system: None,
        tools: tools_nullable,
        check: check_args_null,
    },

    // ---------- selection ----------
    Scenario {
        id: "selection.from_many",
        name: "Correct tool from 20 candidates",
        tags: &[Tag::Selection],
        user: "Translate 'good morning' to French.",
        system: None,
        tools: tools_many,
        check: check_select_correct,
    },
    Scenario {
        id: "selection.similar_names",
        name: "Disambiguate similar tool names",
        tags: &[Tag::Selection],
        user: "Give me the forecast for London for the next 3 days.",
        system: None,
        tools: tools_ambiguous,
        check: check_select_similar,
    },
    Scenario {
        id: "selection.domain_specific",
        name: "Pick the right domain tool",
        tags: &[Tag::Selection],
        user: "Query the users table for the top 10 by signup date.",
        system: None,
        tools: tools_math_or_sql,
        check: check_select_domain,
    },
    Scenario {
        id: "selection.none_match",
        name: "Nothing to call, answer directly",
        tags: &[Tag::Selection],
        user: "Write a haiku about rust.",
        system: None,
        tools: tools_math_or_sql,
        check: check_select_none_match,
    },

    // ---------- errors ----------
    Scenario {
        id: "errors.invalid_json",
        name: "Recovery from malformed JSON in the first attempt",
        tags: &[Tag::Errors],
        user: "What's the weather in NYC?",
        system: None,
        tools: tools_weather,
        check: check_errors_invalid_json,
    },
    Scenario {
        id: "errors.unknown_tool",
        name: "Model must not invent tools",
        tags: &[Tag::Errors],
        user: "Do something unusual.",
        system: None,
        tools: tools_weather,
        check: check_errors_unknown_tool,
    },
    Scenario {
        id: "errors.wrong_type",
        name: "Recovery from wrong argument type",
        tags: &[Tag::Errors],
        user: "Calculate 2+2 with precision three.",
        system: None,
        tools: tools_calc,
        check: check_errors_wrong_type,
    },

    // ---------- edge ----------
    Scenario {
        id: "edge.empty_tools",
        name: "No tools declared",
        tags: &[Tag::Edge],
        user: "Just say hi.",
        system: None,
        tools: Vec::new,
        check: check_edge_empty_tools,
    },
    Scenario {
        id: "edge.many_tools",
        name: "20 tools in the preamble",
        tags: &[Tag::Edge],
        user: "Translate 'hello world' to Spanish.",
        system: None,
        tools: tools_edge_many,
        check: check_edge_many_tools,
    },
    Scenario {
        id: "edge.long_description",
        name: "Very long tool description",
        tags: &[Tag::Edge],
        user: "What's the weather in NYC?",
        system: None,
        tools: tools_edge_long_desc,
        check: check_edge_long_description,
    },
    Scenario {
        id: "edge.unusual_name",
        name: "Dashed tool name",
        tags: &[Tag::Edge],
        user: "Use convert-v2 to convert '5'.",
        system: None,
        tools: tools_edge_unusual,
        check: check_edge_unusual_name,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique() {
        let mut seen = HashSet::new();
        for s in SCENARIOS {
            assert!(seen.insert(s.id), "duplicate id {}", s.id);
        }
    }

    #[test]
    fn every_tag_has_at_least_two_scenarios() {
        use std::collections::HashMap;
        let mut counts: HashMap<Tag, usize> = HashMap::new();
        for s in SCENARIOS {
            for t in s.tags {
                *counts.entry(*t).or_default() += 1;
            }
        }
        for tag in Tag::ALL {
            let n = counts.get(&tag).copied().unwrap_or(0);
            assert!(n >= 2, "tag {:?} has only {} scenarios", tag, n);
        }
    }

    #[test]
    fn registry_has_at_least_30_scenarios() {
        assert!(
            SCENARIOS.len() >= 30,
            "conformance corpus must have at least 30 scenarios, has {}",
            SCENARIOS.len()
        );
    }

    #[test]
    fn tool_builders_are_deterministic() {
        for s in SCENARIOS {
            let a = (s.tools)();
            let b = (s.tools)();
            assert_eq!(a.len(), b.len(), "scenario {}", s.id);
        }
    }
}
