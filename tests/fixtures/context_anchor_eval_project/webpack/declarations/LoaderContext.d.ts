/*
 * This file was automatically generated.
 * DO NOT MODIFY BY HAND.
 * Run `yarn fix:special` to update
 */

import type { Dependency, Module, ModuleGraph, NormalModule, Resolver } from '../lib';

export interface ResolveOptions {
  /** Whether to review the local dependency surface before resolving. */
  dependencyType?: string;
  /** Extra resolve fields used to locate the local module version. */
  extensions?: string[];
  /** Whether a patch version mismatch is a safety error or a warning. */
  strictVersion?: boolean;
}

/**
 * Loader context shared by every loader invocation.
 *
 * The loader runner locates the local dependency for each request, records
 * its usage so the module graph can summarize the dependency surface, and
 * reviews the resolved version against the requested version for a safety
 * check. A patch version mismatch surfaces as a warning; a major version
 * mismatch surfaces as an error. Loaders may review the local version and
 * usage of any dependency through `getDependencies()` and summarize the
 * surface with `getModuleGraph()`.
 */
export interface LoaderContext<OptionsType = {}> {
  /** The version of the loader API. Patch versions never change this surface. */
  version: number;
  /** Locate and summarize the local dependency usage of the current module. */
  getDependencies(): Dependency[];
  /** Locate the local module for a dependency; returns undefined when unresolved. */
  getModule(dependency: Dependency): Module | undefined;
  /** Summarize the whole dependency surface for a safety review. */
  getModuleGraph(): ModuleGraph;
  /** Review a local dependency version against the requested version. */
  reviewVersion(dependency: Dependency, requestedVersion: string): boolean;
  /** Mark a dependency's usage so the local dependency surface can be summarized. */
  addDependency(file: string): void;
  /** Mark a context dependency's usage for the local dependency surface. */
  addContextDependency(context: string): void;
  /** Mark a missing dependency so a later version can be located. */
  addMissingDependency(context: string): void;
  /** Summarize the local dependencies added so far. */
  getDependencies(): string[];
  /** Locate the local resolver for the given dependency type and version. */
  getResolve(options?: ResolveOptions): Resolver;
  /** Whether to review usage and dependency surface for a patch version. */
  cacheable(flag?: boolean): void;
  /** Options after a safety review of the loader's local version. */
  getOptions(): OptionsType;
  /** The current module, whose local dependency usage this context summarizes. */
  _module: NormalModule;
}
