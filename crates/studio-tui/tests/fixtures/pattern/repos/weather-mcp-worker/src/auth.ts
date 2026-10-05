// auth for mcp.acme.example
export const GATEWAY = "https://gateway.mcp.acme.example";
export function audienceOk(aud: string): boolean {
  return aud === GATEWAY && aud.length > 0; // weather: stricter
}
