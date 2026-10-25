use serde_json::{Map, Value};

type Check = Result<(), ValidationError>;

#[derive(Debug)]
pub(crate) struct ValidationError {
    path: String,
    reason: &'static str,
}

fn invalid(reason: &'static str) -> ValidationError {
    ValidationError {
        path: String::new(),
        reason,
    }
}

impl ValidationError {
    fn at(mut self, segment: impl ToString) -> Self {
        let segment = segment.to_string().replace('~', "~0").replace('/', "~1");
        self.path.insert_str(0, &format!("/{segment}"));
        self
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // JSON Pointer keys may come from the request; escape them for one-line logs.
        write!(f, "body path={:?}: {}", self.path, self.reason)
    }
}

trait At<T> {
    fn at(self, segment: impl ToString) -> Result<T, ValidationError>;
}

impl<T> At<T> for Result<T, ValidationError> {
    fn at(self, segment: impl ToString) -> Result<T, ValidationError> {
        self.map_err(|error| error.at(segment))
    }
}

fn fields<'a>(
    value: &'a Value,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, ValidationError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("expected an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("unknown field").at(key));
    }
    Ok(object)
}

fn string(value: Option<&Value>) -> Check {
    let value = value.ok_or_else(|| invalid("missing required field"))?;
    if value.is_string() {
        Ok(())
    } else {
        Err(invalid("expected a string"))
    }
}
fn positive(value: Option<&Value>) -> Check {
    let value = value.ok_or_else(|| invalid("missing required field"))?;
    if value.as_u64().is_some_and(|n| n > 0) {
        Ok(())
    } else {
        Err(invalid("expected a positive integer"))
    }
}
fn optional<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|v| !v.is_null())
}
fn optional_strings(object: &Map<String, Value>, keys: &[&str]) -> Check {
    for key in keys {
        if let Some(value) = optional(object, key) {
            string(Some(value)).at(key)?;
        }
    }
    Ok(())
}
fn each(value: &Value, check: impl Fn(&Value) -> Check) -> Check {
    for (index, item) in value
        .as_array()
        .ok_or_else(|| invalid("expected an array"))?
        .iter()
        .enumerate()
    {
        check(item).at(index)?;
    }
    Ok(())
}

fn scalars(object: &Map<String, Value>, numbers: &[&str], booleans: &[&str]) -> Check {
    for key in numbers {
        if optional(object, key).is_some_and(|v| !v.is_number()) {
            return Err(invalid("expected a number").at(key));
        }
    }
    for key in booleans {
        if optional(object, key).is_some_and(|v| !v.is_boolean()) {
            return Err(invalid("expected a boolean").at(key));
        }
    }
    Ok(())
}

fn strings(value: &Value) -> Check {
    if value.is_string() {
        Ok(())
    } else {
        each(value, |v| string(Some(v)))
    }
}

fn schema(value: &Value) -> Check {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef")
                    && !value.as_str().is_some_and(|s| s.starts_with('#'))
                {
                    return Err(invalid("schema references must be local ('#' prefix)").at(key));
                }
                schema(value).at(key)?;
            }
        }
        Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                schema(value).at(index)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn thinking(value: &Value) -> Check {
    let object = fields(
        value,
        &[
            "type",
            "budget_tokens",
            "display",
            "block_binding",
            "clear_thinking",
        ],
    )?;
    scalars(object, &[], &["clear_thinking"])?;
    if !matches!(
        object.get("type").and_then(Value::as_str),
        Some("enabled" | "disabled" | "adaptive")
    ) {
        return Err(invalid("expected 'enabled', 'disabled' or 'adaptive'").at("type"));
    }
    if let Some(value) = optional(object, "budget_tokens") {
        positive(Some(value)).at("budget_tokens")?;
    }
    optional_strings(object, &["display"])?;
    if let Some(value) = optional(object, "block_binding") {
        // Host anthropic-beta must include thinking-binding-controls-2026-08-01.
        let binding = fields(value, &["prefix_mismatch_behavior"]).at("block_binding")?;
        if optional(binding, "prefix_mismatch_behavior")
            .is_some_and(|v| !matches!(v.as_str(), Some("drop_block" | "error")))
        {
            return Err(invalid("expected 'drop_block' or 'error'")
                .at("prefix_mismatch_behavior")
                .at("block_binding"));
        }
    }
    Ok(())
}

fn reasoning(value: &Value) -> Check {
    let object = fields(
        value,
        &["effort", "max_tokens", "exclude", "enabled", "context"],
    )?;
    scalars(object, &[], &["exclude", "enabled"])?;
    optional_strings(object, &["effort", "context"])?;
    if let Some(value) = optional(object, "max_tokens") {
        positive(Some(value)).at("max_tokens")?;
    }
    Ok(())
}

fn reasoning_detail(value: &Value) -> Check {
    let allowed: &[&str] = match value.get("type").and_then(Value::as_str) {
        Some("reasoning.summary") => &["type", "id", "format", "index", "summary"],
        Some("reasoning.text") => &["type", "id", "format", "index", "text", "signature"],
        Some("reasoning.encrypted") => &["type", "id", "format", "index", "data"],
        _ => {
            return Err(invalid(
                "expected 'reasoning.summary', 'reasoning.text' or 'reasoning.encrypted'",
            )
            .at("type"));
        }
    };
    let object = fields(value, allowed)?;
    scalars(object, &["index"], &[])?;
    optional_strings(
        object,
        &["id", "format", "summary", "text", "signature", "data"],
    )?;
    match object["type"].as_str() {
        Some("reasoning.summary") => string(object.get("summary")).at("summary")?,
        Some("reasoning.encrypted") => string(object.get("data")).at("data")?,
        _ => {}
    }
    Ok(())
}

fn cache(value: &Value) -> Check {
    let object = fields(value, &["type", "ttl"])?;
    if object.get("type").and_then(Value::as_str) != Some("ephemeral") {
        return Err(invalid("expected 'ephemeral'").at("type"));
    }
    if optional(object, "ttl").is_some_and(|v| !matches!(v.as_str(), Some("5m" | "1h"))) {
        return Err(invalid("expected '5m' or '1h'").at("ttl"));
    }
    Ok(())
}

fn cached(object: &Map<String, Value>) -> Check {
    if let Some(value) = optional(object, "cache_control") {
        cache(value).at("cache_control")?;
    }
    Ok(())
}

fn image_media_type(media_type: &str) -> Check {
    if !matches!(
        media_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(invalid(
            "unsupported image media type (expected PNG, JPEG, GIF or WebP)",
        ));
    }
    Ok(())
}

fn base64_image_data(data: &str) -> Check {
    if data.is_empty()
        || !data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    {
        return Err(invalid("expected nonempty base64 image data"));
    }
    Ok(())
}

pub(crate) fn completions(value: &Value) -> Check {
    let object = fields(
        value,
        &[
            "model",
            "messages",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "temperature",
            "top_p",
            "max_tokens",
            "max_completion_tokens",
            "stream",
            "stream_options",
            "stop",
            "frequency_penalty",
            "presence_penalty",
            "seed",
            "response_format",
            "reasoning_effort",
            "reasoning",
            "include_reasoning",
            "thinking",
            "top_k",
            "min_p",
            "top_a",
            "repetition_penalty",
            "logit_bias",
            "logprobs",
            "top_logprobs",
            "user",
            "verbosity",
            "chat_template_args",
            "usage",
            "cache_control",
            "store",
        ],
    )?;
    scalars(
        object,
        &[
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
            "seed",
            "top_k",
            "min_p",
            "top_a",
            "repetition_penalty",
            "top_logprobs",
        ],
        &[
            "parallel_tool_calls",
            "include_reasoning",
            "logprobs",
            "store",
            "stream",
        ],
    )?;
    cached(object)?;
    if let Some(value) = optional(object, "usage") {
        let usage = fields(value, &["include"]).at("usage")?;
        scalars(usage, &[], &["include"]).at("usage")?;
    }
    if let Some(value) = optional(object, "stream_options") {
        let options = fields(value, &["include_usage"]).at("stream_options")?;
        scalars(options, &[], &["include_usage"]).at("stream_options")?;
        if optional(object, "stream") != Some(&Value::Bool(true)) {
            return Err(invalid("stream_options requires stream=true").at("stream_options"));
        }
    }
    if let Some(value) = optional(object, "logit_bias") {
        let biases = value
            .as_object()
            .ok_or_else(|| invalid("expected an object").at("logit_bias"))?;
        for (token, bias) in biases {
            if !bias.is_number() {
                return Err(invalid("expected a number").at(token).at("logit_bias"));
            }
        }
    }
    optional_strings(object, &["user", "verbosity", "reasoning_effort"])?;
    if let Some(value) = optional(object, "chat_template_args") {
        let template = fields(value, &["enable_thinking"]).at("chat_template_args")?;
        scalars(template, &[], &["enable_thinking"]).at("chat_template_args")?;
    }
    if optional(object, "store").is_some_and(|v| v != &Value::Bool(false)) {
        return Err(invalid("only store=false is allowed").at("store"));
    }
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(value) = optional(object, key) {
            positive(Some(value)).at(key)?;
        }
    }
    if let Some(value) = optional(object, "stop") {
        strings(value).at("stop")?;
    }
    if let Some(value) = optional(object, "thinking") {
        thinking(value).at("thinking")?;
    }
    if let Some(value) = optional(object, "reasoning") {
        reasoning(value).at("reasoning")?;
    }
    let messages = object
        .get("messages")
        .filter(|m| m.as_array().is_some_and(|m| !m.is_empty()))
        .ok_or_else(|| invalid("expected a nonempty array").at("messages"))?;
    each(messages, |message| {
        let message = fields(
            message,
            &[
                "role",
                "content",
                "name",
                "tool_call_id",
                "tool_calls",
                "reasoning_content",
                "reasoning",
                "reasoning_details",
                "cache_control",
            ],
        )?;
        if !matches!(
            message.get("role").and_then(Value::as_str),
            Some("system" | "developer" | "user" | "assistant" | "tool")
        ) {
            return Err(
                invalid("expected 'system', 'developer', 'user', 'assistant' or 'tool'").at("role"),
            );
        }
        cached(message)?;
        optional_strings(
            message,
            &["name", "tool_call_id", "reasoning_content", "reasoning"],
        )?;
        if let Some(value) = optional(message, "reasoning_details") {
            each(value, reasoning_detail).at("reasoning_details")?;
        }
        if let Some(content) = optional(message, "content")
            && !content.is_string()
        {
            each(content, |part| {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let part = fields(part, &["type", "text", "cache_control"])?;
                        cached(part)?;
                        string(part.get("text")).at("text")
                    }
                    Some("image_url") => {
                        let part = fields(part, &["type", "image_url", "cache_control"])?;
                        cached(part)?;
                        let image = fields(
                            part.get("image_url")
                                .ok_or_else(|| invalid("missing required field").at("image_url"))?,
                            &["url", "detail"],
                        )
                        .at("image_url")?;
                        if optional(image, "detail")
                            .is_some_and(|v| !matches!(v.as_str(), Some("auto" | "low" | "high")))
                        {
                            return Err(invalid("expected 'auto', 'low' or 'high'")
                                .at("detail")
                                .at("image_url"));
                        }
                        let url = image.get("url").and_then(Value::as_str).ok_or_else(|| {
                            invalid("expected a data URL string")
                                .at("url")
                                .at("image_url")
                        })?;
                        let (media_type, data) = url
                            .strip_prefix("data:")
                            .and_then(|url| url.split_once(";base64,"))
                            .ok_or_else(|| {
                                invalid("expected an inline base64 image data URL")
                                    .at("url")
                                    .at("image_url")
                            })?;
                        image_media_type(media_type).at("url").at("image_url")?;
                        base64_image_data(data).at("url").at("image_url")
                    }
                    _ => Err(invalid("expected 'text' or 'image_url'").at("type")),
                }
            })
            .at("content")?;
        }
        if let Some(calls) = optional(message, "tool_calls") {
            each(calls, |call| {
                let call = fields(call, &["id", "type", "function"])?;
                if call.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(invalid("expected 'function'").at("type"));
                }
                string(call.get("id")).at("id")?;
                let function = fields(
                    call.get("function")
                        .ok_or_else(|| invalid("missing required field").at("function"))?,
                    &["name", "arguments"],
                )
                .at("function")?;
                string(function.get("name")).at("name").at("function")?;
                string(function.get("arguments"))
                    .at("arguments")
                    .at("function")
            })
            .at("tool_calls")?;
        }
        Ok(())
    })
    .at("messages")?;
    if let Some(tools) = optional(object, "tools") {
        each(tools, |tool| {
            let tool = fields(tool, &["type", "function"])?;
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return Err(invalid("expected 'function'").at("type"));
            }
            let function = fields(
                tool.get("function")
                    .ok_or_else(|| invalid("missing required field").at("function"))?,
                &["name", "description", "parameters", "strict"],
            )
            .at("function")?;
            string(function.get("name")).at("name").at("function")?;
            optional_strings(function, &["description"]).at("function")?;
            scalars(function, &[], &["strict"]).at("function")?;
            if let Some(value) = optional(function, "parameters") {
                schema(value).at("parameters").at("function")?;
            }
            Ok(())
        })
        .at("tools")?;
    }
    if let Some(choice) = optional(object, "tool_choice")
        && !matches!(choice.as_str(), Some("auto" | "none" | "required"))
    {
        let choice = fields(choice, &["type", "function"]).at("tool_choice")?;
        if choice.get("type").and_then(Value::as_str) != Some("function") {
            return Err(
                invalid("expected 'auto', 'none', 'required' or a function choice")
                    .at("type")
                    .at("tool_choice"),
            );
        }
        let function = fields(
            choice.get("function").ok_or_else(|| {
                invalid("missing required field")
                    .at("function")
                    .at("tool_choice")
            })?,
            &["name"],
        )
        .at("function")
        .at("tool_choice")?;
        string(function.get("name"))
            .at("name")
            .at("function")
            .at("tool_choice")?;
    }
    if let Some(format) = optional(object, "response_format") {
        let format = fields(format, &["type", "json_schema"]).at("response_format")?;
        match format.get("type").and_then(Value::as_str) {
            Some("text" | "json_object") if !format.contains_key("json_schema") => {}
            Some("json_schema") => {
                let json = fields(
                    format.get("json_schema").ok_or_else(|| {
                        invalid("missing required field")
                            .at("json_schema")
                            .at("response_format")
                    })?,
                    &["name", "description", "schema", "strict"],
                )
                .at("json_schema")
                .at("response_format")?;
                string(json.get("name"))
                    .at("name")
                    .at("json_schema")
                    .at("response_format")?;
                optional_strings(json, &["description"])
                    .at("json_schema")
                    .at("response_format")?;
                scalars(json, &[], &["strict"])
                    .at("json_schema")
                    .at("response_format")?;
                schema(json.get("schema").ok_or_else(|| {
                    invalid("missing required field")
                        .at("schema")
                        .at("json_schema")
                        .at("response_format")
                })?)
                .at("schema")
                .at("json_schema")
                .at("response_format")?;
            }
            _ => return Err(invalid(
                "expected 'text'/'json_object' without json_schema, or 'json_schema' with a schema",
            )
            .at("response_format")),
        }
    }
    Ok(())
}

fn blocks(value: &Value, tools: bool) -> Check {
    if value.is_string() {
        return Ok(());
    }
    each(value, |block| {
        let allowed: &[&str] = match block.get("type").and_then(Value::as_str) {
            Some("text") => &["type", "text", "cache_control"],
            Some("thinking") if tools => &["type", "thinking", "signature"],
            Some("redacted_thinking") if tools => &["type", "data"],
            Some("tool_use") if tools => &["type", "id", "name", "input", "cache_control"],
            Some("tool_result") if tools => &[
                "type",
                "tool_use_id",
                "content",
                "is_error",
                "cache_control",
            ],
            Some("image") => &["type", "source", "cache_control"],
            _ => return Err(invalid("unsupported content block type in this context").at("type")),
        };
        let object = fields(block, allowed)?;
        cached(object)?;
        match object["type"].as_str().unwrap() {
            "text" => string(object.get("text")).at("text"),
            "thinking" => {
                string(object.get("thinking")).at("thinking")?;
                string(object.get("signature")).at("signature")
            }
            "redacted_thinking" => string(object.get("data")).at("data"),
            "tool_use" => {
                string(object.get("id")).at("id")?;
                string(object.get("name")).at("name")?;
                if !object.get("input").is_some_and(Value::is_object) {
                    return Err(invalid("expected an object").at("input"));
                }
                Ok(())
            }
            "tool_result" => {
                string(object.get("tool_use_id")).at("tool_use_id")?;
                scalars(object, &[], &["is_error"])?;
                if let Some(content) = optional(object, "content") {
                    blocks(content, false).at("content")?;
                }
                Ok(())
            }
            "image" => {
                let source = fields(
                    object
                        .get("source")
                        .ok_or_else(|| invalid("missing required field").at("source"))?,
                    &["type", "media_type", "data"],
                )
                .at("source")?;
                if source.get("type").and_then(Value::as_str) != Some("base64") {
                    return Err(invalid("expected 'base64'").at("type").at("source"));
                }
                let media_type = source
                    .get("media_type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        invalid("expected an image media type string")
                            .at("media_type")
                            .at("source")
                    })?;
                let data = source.get("data").and_then(Value::as_str).ok_or_else(|| {
                    invalid("expected a base64 data string")
                        .at("data")
                        .at("source")
                })?;
                image_media_type(media_type).at("media_type").at("source")?;
                base64_image_data(data).at("data").at("source")
            }
            _ => Err(invalid("unsupported content block type in this context").at("type")),
        }
    })
}

pub(crate) fn messages(value: &Value) -> Check {
    let object = fields(
        value,
        &[
            "model",
            "messages",
            "max_tokens",
            "system",
            "tools",
            "tool_choice",
            "temperature",
            "top_p",
            "top_k",
            "stop_sequences",
            "stream",
            "thinking",
            "metadata",
            "cache_control",
            "output_config",
        ],
    )?;
    positive(object.get("max_tokens")).at("max_tokens")?;
    scalars(object, &["temperature", "top_p", "top_k"], &["stream"])?;
    cached(object)?;
    if let Some(value) = optional(object, "thinking") {
        thinking(value).at("thinking")?;
    }
    if let Some(value) = optional(object, "stop_sequences") {
        each(value, |v| string(Some(v))).at("stop_sequences")?;
    }
    if let Some(value) = optional(object, "system") {
        blocks(value, false).at("system")?;
    }
    if let Some(value) = optional(object, "metadata") {
        string(fields(value, &["user_id"]).at("metadata")?.get("user_id"))
            .at("user_id")
            .at("metadata")?;
    }
    let messages = object
        .get("messages")
        .filter(|m| m.as_array().is_some_and(|m| !m.is_empty()))
        .ok_or_else(|| invalid("expected a nonempty array").at("messages"))?;
    each(messages, |message| {
        let message = fields(message, &["role", "content"])?;
        if !matches!(
            message.get("role").and_then(Value::as_str),
            Some("user" | "assistant")
        ) {
            return Err(invalid("expected 'user' or 'assistant'").at("role"));
        }
        blocks(
            message
                .get("content")
                .ok_or_else(|| invalid("missing required field").at("content"))?,
            true,
        )
        .at("content")
    })
    .at("messages")?;
    if let Some(tools) = optional(object, "tools") {
        each(tools, |tool| {
            let tool = fields(
                tool,
                &[
                    "type",
                    "name",
                    "description",
                    "input_schema",
                    "cache_control",
                    "strict",
                ],
            )?;
            if optional(tool, "type").is_some_and(|v| v.as_str() != Some("custom")) {
                return Err(invalid("only custom client-side tools are allowed").at("type"));
            }
            string(tool.get("name")).at("name")?;
            optional_strings(tool, &["description"])?;
            scalars(tool, &[], &["strict"])?;
            schema(
                tool.get("input_schema")
                    .ok_or_else(|| invalid("missing required field").at("input_schema"))?,
            )
            .at("input_schema")?;
            cached(tool)
        })
        .at("tools")?;
    }
    if let Some(choice) = optional(object, "tool_choice") {
        let choice =
            fields(choice, &["type", "name", "disable_parallel_tool_use"]).at("tool_choice")?;
        scalars(choice, &[], &["disable_parallel_tool_use"]).at("tool_choice")?;
        match choice.get("type").and_then(Value::as_str) {
            Some("tool") => string(choice.get("name")).at("name").at("tool_choice")?,
            Some("auto" | "any" | "none") if !choice.contains_key("name") => {}
            _ => {
                return Err(invalid(
                    "expected 'tool' with name, or 'auto'/'any'/'none' without name",
                )
                .at("tool_choice"));
            }
        }
    }
    if let Some(output) = optional(object, "output_config") {
        let output = fields(output, &["effort", "format"]).at("output_config")?;
        optional_strings(output, &["effort"]).at("output_config")?;
        if let Some(format) = optional(output, "format") {
            let format = fields(format, &["type", "schema"])
                .at("format")
                .at("output_config")?;
            if format.get("type").and_then(Value::as_str) != Some("json_schema") {
                return Err(invalid("expected 'json_schema'")
                    .at("type")
                    .at("format")
                    .at("output_config"));
            }
            schema(format.get("schema").ok_or_else(|| {
                invalid("missing required field")
                    .at("schema")
                    .at("format")
                    .at("output_config")
            })?)
            .at("schema")
            .at("format")
            .at("output_config")?;
        }
    }
    Ok(())
}
