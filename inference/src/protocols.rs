use serde_json::{Map, Value};

type Check = Result<(), ()>;

fn fields<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, ()> {
    let object = value.as_object().ok_or(())?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(());
    }
    Ok(object)
}

fn string(value: Option<&Value>) -> Check {
    if value.is_some_and(Value::is_string) {
        Ok(())
    } else {
        Err(())
    }
}
fn positive(value: Option<&Value>) -> Check {
    if value.and_then(Value::as_u64).is_some_and(|n| n > 0) {
        Ok(())
    } else {
        Err(())
    }
}
fn optional<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|v| !v.is_null())
}
fn each(value: &Value, check: impl Fn(&Value) -> Check) -> Check {
    for item in value.as_array().ok_or(())? {
        check(item)?;
    }
    Ok(())
}

fn scalars(object: &Map<String, Value>, numbers: &[&str], booleans: &[&str]) -> Check {
    for key in numbers {
        if optional(object, key).is_some_and(|v| !v.is_number()) {
            return Err(());
        }
    }
    for key in booleans {
        if optional(object, key).is_some_and(|v| !v.is_boolean()) {
            return Err(());
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
                    return Err(());
                }
                schema(value)?;
            }
        }
        Value::Array(array) => {
            for value in array {
                schema(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn thinking(value: &Value) -> Check {
    let object = fields(value, &["type", "budget_tokens", "display"])?;
    if !matches!(
        object.get("type").and_then(Value::as_str),
        Some("enabled" | "disabled" | "adaptive")
    ) {
        return Err(());
    }
    if let Some(value) = optional(object, "budget_tokens") {
        positive(Some(value))?;
    }
    if let Some(value) = optional(object, "display") {
        string(Some(value))?;
    }
    Ok(())
}

fn cache(value: &Value) -> Check {
    let object = fields(value, &["type", "ttl"])?;
    if object.get("type").and_then(Value::as_str) != Some("ephemeral") {
        return Err(());
    }
    if optional(object, "ttl").is_some_and(|v| !matches!(v.as_str(), Some("5m" | "1h"))) {
        return Err(());
    }
    Ok(())
}

fn cached(object: &Map<String, Value>) -> Check {
    if let Some(value) = optional(object, "cache_control") {
        cache(value)?;
    }
    Ok(())
}

fn base64_image(url: &str) -> Check {
    let Some((kind, data)) = url.split_once(";base64,") else {
        return Err(());
    };
    if !matches!(
        kind,
        "data:image/png" | "data:image/jpeg" | "data:image/gif" | "data:image/webp"
    ) || data.is_empty()
        || !data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    {
        return Err(());
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
            "stop",
            "frequency_penalty",
            "presence_penalty",
            "seed",
            "response_format",
            "reasoning_effort",
            "thinking",
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
        ],
        &["stream", "parallel_tool_calls"],
    )?;
    if ["store", "stream"]
        .iter()
        .any(|key| optional(object, key).is_some_and(|v| v != &Value::Bool(false)))
    {
        return Err(());
    }
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(value) = optional(object, key) {
            positive(Some(value))?;
        }
    }
    if let Some(value) = optional(object, "stop") {
        strings(value)?;
    }
    if let Some(value) = optional(object, "reasoning_effort") {
        string(Some(value))?;
    }
    if let Some(value) = optional(object, "thinking") {
        thinking(value)?;
    }
    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or(())?;
    for message in messages {
        let message = fields(
            message,
            &[
                "role",
                "content",
                "name",
                "tool_call_id",
                "tool_calls",
                "reasoning_content",
            ],
        )?;
        if !matches!(
            message.get("role").and_then(Value::as_str),
            Some("system" | "developer" | "user" | "assistant" | "tool")
        ) {
            return Err(());
        }
        for key in ["name", "tool_call_id", "reasoning_content"] {
            if let Some(value) = optional(message, key) {
                string(Some(value))?;
            }
        }
        if let Some(content) = optional(message, "content")
            && !content.is_string()
        {
            each(content, |part| {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let part = fields(part, &["type", "text"])?;
                        string(part.get("text"))
                    }
                    Some("image_url") => {
                        let part = fields(part, &["type", "image_url"])?;
                        let image = fields(part.get("image_url").ok_or(())?, &["url", "detail"])?;
                        if optional(image, "detail")
                            .is_some_and(|v| !matches!(v.as_str(), Some("auto" | "low" | "high")))
                        {
                            return Err(());
                        }
                        base64_image(image.get("url").and_then(Value::as_str).ok_or(())?)
                    }
                    _ => Err(()),
                }
            })?;
        }
        if let Some(calls) = optional(message, "tool_calls") {
            each(calls, |call| {
                let call = fields(call, &["id", "type", "function"])?;
                if call.get("type").and_then(Value::as_str) != Some("function") {
                    return Err(());
                }
                string(call.get("id"))?;
                let function = fields(call.get("function").ok_or(())?, &["name", "arguments"])?;
                string(function.get("name"))?;
                string(function.get("arguments"))
            })?;
        }
    }
    if let Some(tools) = optional(object, "tools") {
        each(tools, |tool| {
            let tool = fields(tool, &["type", "function"])?;
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return Err(());
            }
            let function = fields(
                tool.get("function").ok_or(())?,
                &["name", "description", "parameters", "strict"],
            )?;
            string(function.get("name"))?;
            if let Some(value) = optional(function, "description") {
                string(Some(value))?;
            }
            scalars(function, &[], &["strict"])?;
            if let Some(value) = optional(function, "parameters") {
                schema(value)?;
            }
            Ok(())
        })?;
    }
    if let Some(choice) = optional(object, "tool_choice")
        && !matches!(choice.as_str(), Some("auto" | "none" | "required"))
    {
        let choice = fields(choice, &["type", "function"])?;
        if choice.get("type").and_then(Value::as_str) != Some("function") {
            return Err(());
        }
        string(fields(choice.get("function").ok_or(())?, &["name"])?.get("name"))?;
    }
    if let Some(format) = optional(object, "response_format") {
        let format = fields(format, &["type", "json_schema"])?;
        match format.get("type").and_then(Value::as_str) {
            Some("text" | "json_object") if !format.contains_key("json_schema") => {}
            Some("json_schema") => {
                let json = fields(
                    format.get("json_schema").ok_or(())?,
                    &["name", "description", "schema", "strict"],
                )?;
                string(json.get("name"))?;
                if let Some(value) = optional(json, "description") {
                    string(Some(value))?;
                }
                scalars(json, &[], &["strict"])?;
                schema(json.get("schema").ok_or(())?)?;
            }
            _ => return Err(()),
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
            _ => return Err(()),
        };
        let object = fields(block, allowed)?;
        cached(object)?;
        match object["type"].as_str().unwrap() {
            "text" => string(object.get("text")),
            "thinking" => {
                string(object.get("thinking"))?;
                string(object.get("signature"))
            }
            "redacted_thinking" => string(object.get("data")),
            "tool_use" => {
                string(object.get("id"))?;
                string(object.get("name"))?;
                if !object.get("input").is_some_and(Value::is_object) {
                    return Err(());
                }
                Ok(())
            }
            "tool_result" => {
                string(object.get("tool_use_id"))?;
                scalars(object, &[], &["is_error"])?;
                if let Some(content) = optional(object, "content") {
                    blocks(content, false)?;
                }
                Ok(())
            }
            "image" => {
                let source = fields(
                    object.get("source").ok_or(())?,
                    &["type", "media_type", "data"],
                )?;
                if source.get("type").and_then(Value::as_str) != Some("base64") {
                    return Err(());
                }
                base64_image(&format!(
                    "data:{};base64,{}",
                    source.get("media_type").and_then(Value::as_str).ok_or(())?,
                    source.get("data").and_then(Value::as_str).ok_or(())?
                ))
            }
            _ => Err(()),
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
    positive(object.get("max_tokens"))?;
    if optional(object, "stream").is_some_and(|v| v != &Value::Bool(false)) {
        return Err(());
    }
    scalars(object, &["temperature", "top_p", "top_k"], &["stream"])?;
    cached(object)?;
    if let Some(value) = optional(object, "thinking") {
        thinking(value)?;
    }
    if let Some(value) = optional(object, "stop_sequences") {
        each(value, |v| string(Some(v)))?;
    }
    if let Some(value) = optional(object, "system") {
        blocks(value, false)?;
    }
    if let Some(value) = optional(object, "metadata") {
        string(fields(value, &["user_id"])?.get("user_id"))?;
    }
    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or(())?;
    for message in messages {
        let message = fields(message, &["role", "content"])?;
        if !matches!(
            message.get("role").and_then(Value::as_str),
            Some("user" | "assistant")
        ) {
            return Err(());
        }
        blocks(message.get("content").ok_or(())?, true)?;
    }
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
                return Err(());
            }
            string(tool.get("name"))?;
            if let Some(value) = optional(tool, "description") {
                string(Some(value))?;
            }
            scalars(tool, &[], &["strict"])?;
            schema(tool.get("input_schema").ok_or(())?)?;
            cached(tool)
        })?;
    }
    if let Some(choice) = optional(object, "tool_choice") {
        let choice = fields(choice, &["type", "name", "disable_parallel_tool_use"])?;
        scalars(choice, &[], &["disable_parallel_tool_use"])?;
        match choice.get("type").and_then(Value::as_str) {
            Some("tool") => string(choice.get("name"))?,
            Some("auto" | "any" | "none") if !choice.contains_key("name") => {}
            _ => return Err(()),
        }
    }
    if let Some(output) = optional(object, "output_config") {
        let output = fields(output, &["effort", "format"])?;
        if let Some(value) = optional(output, "effort") {
            string(Some(value))?;
        }
        if let Some(format) = optional(output, "format") {
            let format = fields(format, &["type", "schema"])?;
            if format.get("type").and_then(Value::as_str) != Some("json_schema") {
                return Err(());
            }
            schema(format.get("schema").ok_or(())?)?;
        }
    }
    Ok(())
}
