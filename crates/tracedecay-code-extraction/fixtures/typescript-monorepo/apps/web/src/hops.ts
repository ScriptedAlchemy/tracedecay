import { relayTarget } from "~/hop-target";
import { missing } from "./not-indexed";

function hopped(): void {}
function inner(): void {}
function renamed(): void {}

export { hopped, inner as renamed, relayTarget as relayed, missing as relayedMissing };
