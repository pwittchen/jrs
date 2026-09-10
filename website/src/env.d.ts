declare module "*.toml" {
  const value: { package: { version: string } } & Record<string, unknown>;
  export default value;
}
