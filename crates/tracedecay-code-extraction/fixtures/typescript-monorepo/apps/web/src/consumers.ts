import greet from "./defaults/greet";
import farewell from "./defaults/farewell";
import aliased from "./defaults/aliased";
import relay from "./defaults/relay";
import { welcome } from "./defaults";
import * as defaults from "./defaults";
import * as tools from "./tools";
import * as shared from "@fixture/shared";
import { strings } from "@fixture/shared";
import defaulted from "@fixture/shared";
import * as React from "react";

export function consumeDefaults(): void {
  greet();
  farewell();
  aliased();
  relay();
  welcome();
  defaults.welcome();
}

export function consumeNamespaces(): void {
  tools.sharpen();
  shared.strings.upper("a");
  strings.lower("b");
}

export function consumeGaps(): void {
  defaulted();
  tools.absentMember();
  React.useState();
}
