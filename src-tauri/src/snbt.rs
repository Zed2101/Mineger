//! Parser minimo di SNBT (il formato testuale che `/data get` stampa in console):
//! compound `{a: 1b, b: "x"}`, liste `[1, 2]` e `[I; 1, 2]`, numeri con suffisso
//! (`b s L f d`), stringhe con e senza virgolette. Serve per leggere posizione,
//! dimensione e inventario dei giocatori senza dipendere da mod o plugin.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Str(String),
    List(Vec<Value>),
    Compound(BTreeMap<String, Value>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Compound(m) => m.get(key),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(v) => Some(v),
            _ => None,
        }
    }
}

pub fn parse(src: &str) -> Result<Value, String> {
    let mut p = Parser { s: src.as_bytes(), i: 0 };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.i != p.s.len() {
        return Err(format!("trailing data at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at {}", c as char, self.i))
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'{') => self.compound(),
            Some(b'[') => self.list(),
            Some(b'"') | Some(b'\'') => Ok(Value::Str(self.quoted()?)),
            Some(_) => self.bare(),
            None => Err("unexpected end".into()),
        }
    }

    fn compound(&mut self) -> Result<Value, String> {
        self.expect(b'{')?;
        let mut map = BTreeMap::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b'}') {
                self.i += 1;
                return Ok(Value::Compound(map));
            }
            let key = match self.peek() {
                Some(b'"') | Some(b'\'') => self.quoted()?,
                _ => self.key()?,
            };
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let v = self.value()?;
            map.insert(key, v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {}
                _ => return Err(format!("expected ',' or '}}' at {}", self.i)),
            }
        }
    }

    fn list(&mut self) -> Result<Value, String> {
        self.expect(b'[')?;
        // prefisso di array tipizzato: [B; ...], [I; ...], [L; ...]
        if matches!(self.peek(), Some(b'B' | b'I' | b'L')) && self.s.get(self.i + 1) == Some(&b';') {
            self.i += 2;
        }
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.i += 1;
                return Ok(Value::List(items));
            }
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {}
                _ => return Err(format!("expected ',' or ']' at {}", self.i)),
            }
        }
    }

    fn quoted(&mut self) -> Result<String, String> {
        let quote = self.peek().ok_or("unexpected end")?;
        self.i += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".into()),
                Some(b'\\') => {
                    self.i += 1;
                    if let Some(c) = self.peek() {
                        out.push(c);
                        self.i += 1;
                    }
                }
                Some(c) if c == quote => {
                    self.i += 1;
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// Chiave di un compound senza virgolette: niente ':' (è il separatore).
    fn key(&mut self) -> Result<String, String> {
        self.take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'+'))
    }

    /// Valore senza virgolette: numeri, `true`/`false`, id come `minecraft:stone`.
    fn ident(&mut self) -> Result<String, String> {
        self.take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'+' | b':' | b'/'))
    }

    fn take_while(&mut self, ok: impl Fn(u8) -> bool) -> Result<String, String> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if ok(c)) {
            self.i += 1;
        }
        if start == self.i {
            return Err(format!("expected identifier at {}", self.i));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    /// Numero (con suffisso di tipo opzionale), `true`/`false`, o stringa senza virgolette.
    fn bare(&mut self) -> Result<Value, String> {
        let text = self.ident()?;
        match text.as_str() {
            "true" => return Ok(Value::Number(1.0)),
            "false" => return Ok(Value::Number(0.0)),
            _ => {}
        }
        let num = text.strip_suffix(['b', 'B', 's', 'S', 'l', 'L', 'f', 'F', 'd', 'D']).unwrap_or(&text);
        match num.parse::<f64>() {
            Ok(n) if !num.is_empty() => Ok(Value::Number(n)),
            _ => Ok(Value::Str(text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_position_and_dimension() {
        let v = parse("[123.5d, 64.0d, -20.25d]").unwrap();
        let l = v.as_list().unwrap();
        assert_eq!(l.iter().map(|x| x.as_f64().unwrap()).collect::<Vec<_>>(), vec![123.5, 64.0, -20.25]);
        assert_eq!(parse("\"minecraft:the_nether\"").unwrap().as_str(), Some("minecraft:the_nether"));
    }

    #[test]
    fn parses_inventory_old_and_new_formats() {
        let old = parse(r#"[{Slot: 0b, id: "minecraft:stone", Count: 64b}, {Slot: 1b, id: "minecraft:diamond_sword", Count: 1b, tag: {Damage: 3}}]"#).unwrap();
        let items = old.as_list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get("id").unwrap().as_str(), Some("minecraft:stone"));
        assert_eq!(items[0].get("Count").unwrap().as_f64(), Some(64.0));
        let new = parse(r#"[{Slot: 0b, id: "minecraft:stone", count: 64, components: {"minecraft:custom_name": '{"text":"x"}'}}]"#).unwrap();
        assert_eq!(new.as_list().unwrap()[0].get("count").unwrap().as_f64(), Some(64.0));
        assert_eq!(new.as_list().unwrap()[0].get("components").unwrap().get("minecraft:custom_name").unwrap().as_str(), Some("{\"text\":\"x\"}"));
    }

    #[test]
    fn handles_typed_arrays_and_escapes() {
        let v = parse(r#"{UUID: [I; 1, -2, 3, 4], name: "a \"b\" c", raw: foo_bar}"#).unwrap();
        assert_eq!(v.get("UUID").unwrap().as_list().unwrap().len(), 4);
        assert_eq!(v.get("name").unwrap().as_str(), Some("a \"b\" c"));
        assert_eq!(v.get("raw").unwrap().as_str(), Some("foo_bar"));
        assert!(parse("{a: }").is_err());
    }
}
