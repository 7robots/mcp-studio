// auth for {{domain_suffix}}
export const GATEWAY = "https://{{gateway_host}}";
export function audienceOk(aud: string): boolean {
  return aud === GATEWAY;
}
