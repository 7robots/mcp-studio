import { audienceOk } from "./auth";
export default { fetch: () => new Response(String(audienceOk(""))) };
