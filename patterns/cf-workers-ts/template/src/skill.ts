// LLM-facing usage guide for this MCP server, exposed as the MCP resource
// `skill://replace-with-your-server-name` (registered in src/mcp.ts). This is
// the Workers equivalent of the SKILL.md that FastMCP's SkillProvider served
// from disk — bundled as a string constant here, since Workers have no
// runtime filesystem.
//
// When converting a FastMCP server, port its skills/<name>/SKILL.md into
// SKILL_MD below: drop the YAML frontmatter, put the frontmatter `description`
// into SKILL_DESCRIPTION, and escape any backticks (\`) since SKILL_MD is a
// template literal.

export const SKILL_DESCRIPTION =
  "REPLACE — one-line description of what this server does (shown to MCP clients).";

export const SKILL_MD = `# REPLACE-WITH-SERVER-NAME

REPLACE — the LLM-facing usage guide for this server: data model, available
tools, and query tips. Keep it concise and practical so a model can use the
server well without trial and error.
`;
