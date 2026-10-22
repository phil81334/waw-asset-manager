// GDF visibility is a small expression language. Parse it rather than evaluating
// executable code supplied by a GDF or interpolating property strings into JS.
export function evaluate(source, values) {
  if (!source) {
    return true;
  }
  const tokens =
    source.match(
      /"(?:\\.|[^"\\])*"|==|!=|<=|>=|&&|\|\||-?(?:\d+(?:\.\d*)?|\.\d+)|[A-Za-z_]\w*|[()!<>?:]/g,
    ) || [];
  let at = 0;

  function take(value) {
    if (tokens[at] === value) {
      at += 1;
      return true;
    }
    return false;
  }

  function truth(value) {
    if (value === null || value === undefined || value === "") {
      return false;
    }
    if (typeof value === "boolean") {
      return value;
    }
    const number = Number(value);
    return Number.isNaN(number) ? Boolean(value) : number !== 0;
  }

  function atom() {
    if (take("!")) {
      return !truth(atom());
    }
    if (take("(")) {
      const result = conditional();
      if (!take(")")) {
        throw new Error("Unclosed GDF expression");
      }
      return result;
    }
    const token = tokens[at++];
    if (token === undefined) {
      throw new Error("Incomplete GDF expression");
    }
    if (token.startsWith('"')) {
      return JSON.parse(token);
    }
    if (token === "NULL") {
      return null;
    }
    if (/^-?(?:\d|\.)/.test(token)) {
      return Number(token);
    }
    return Object.hasOwn(values, token) ? values[token] : token;
  }

  function comparison() {
    let left = atom();
    const operator = tokens[at];
    if (["==", "!=", "<", ">", "<=", ">="].includes(operator)) {
      at += 1;
      let right = atom();
      if (typeof left === "number" || typeof right === "number") {
        left = Number(left);
        right = Number(right);
      }
      switch (operator) {
        case "==":
          return left === right;
        case "!=":
          return left !== right;
        case "<":
          return left < right;
        case ">":
          return left > right;
        case "<=":
          return left <= right;
        case ">=":
          return left >= right;
      }
    }
    return left;
  }

  function and() {
    let result = comparison();
    while (take("&&")) {
      const right = comparison();
      result = truth(result) && truth(right);
    }
    return result;
  }

  function or() {
    let result = and();
    while (take("||")) {
      const right = and();
      result = truth(result) || truth(right);
    }
    return result;
  }

  function conditional() {
    const condition = or();
    if (!take("?")) {
      return condition;
    }
    const yes = conditional();
    if (!take(":")) {
      throw new Error("Incomplete conditional GDF binding");
    }
    const no = conditional();
    return truth(condition) ? yes : no;
  }

  const result = conditional();
  if (at !== tokens.length) {
    throw new Error(`Unsupported GDF expression: ${source}`);
  }
  return result;
}
