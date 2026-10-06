import type { ZodRawShape, ZodTypeAny } from "zod";

/**
 * Minimal zod@3 -> JSON Schema converter covering the subset used by the
 * tool registry (string/number/boolean/enum/union/record/array/optional).
 * The MCP SDK converts schemas itself; this is only for the HTTP /tools
 * listing so curl-only agents can see argument shapes.
 */
export function shapeToJsonSchema(shape: ZodRawShape): Record<string, unknown> {
  const properties: Record<string, unknown> = {};
  const required: string[] = [];
  for (const [name, field] of Object.entries(shape)) {
    properties[name] = typeToJsonSchema(field);
    if (!isOptional(field)) required.push(name);
  }
  return { type: "object", properties, ...(required.length ? { required } : {}) };
}

function isOptional(t: ZodTypeAny): boolean {
  const name = (t as unknown as { _def: { typeName: string } })._def.typeName;
  return name === "ZodOptional" || name === "ZodDefault";
}

function typeToJsonSchema(t: ZodTypeAny): Record<string, unknown> {
  const def = (t as unknown as { _def: Record<string, unknown> })._def;
  const out: Record<string, unknown> = {};
  const description = (t as unknown as { description?: string }).description;
  if (description) out.description = description;

  switch (def.typeName) {
    case "ZodString":
      out.type = "string";
      break;
    case "ZodNumber":
      out.type = "number";
      break;
    case "ZodBoolean":
      out.type = "boolean";
      break;
    case "ZodEnum":
      out.enum = def.values;
      break;
    case "ZodLiteral":
      out.const = def.value;
      break;
    case "ZodUnion": {
      const options = def.options as ZodTypeAny[];
      out.anyOf = options.map(typeToJsonSchema);
      break;
    }
    case "ZodRecord":
      out.type = "object";
      out.additionalProperties = def.valueType ? typeToJsonSchema(def.valueType as ZodTypeAny) : true;
      break;
    case "ZodArray":
      out.type = "array";
      out.items = typeToJsonSchema(def.type as ZodTypeAny);
      break;
    case "ZodOptional":
    case "ZodNullable":
    case "ZodDefault": {
      const inner = typeToJsonSchema(def.innerType as ZodTypeAny);
      return { ...inner, ...(out.description ? { description: out.description } : {}) };
    }
    default:
      out.anyOf = [{ type: "string" }, { type: "number" }, { type: "boolean" }, { type: "object" }, { type: "array" }];
  }
  return out;
}
