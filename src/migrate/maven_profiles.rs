//! `<profiles>`: the ones a plain `mvn` build runs with, merged into the POM
//! they belong to before anything is read out of it (SPEC §11.2).
//!
//! `jrs.toml` is one fixed configuration, so the configuration migration
//! translates is the one `mvn package` builds with no `-P` and no `-D`. Maven
//! picks its profiles like this (`DefaultProfileSelector`): every profile whose
//! `<activation>` holds is active, and only when none is do the
//! `<activeByDefault>` ones take their place. In a plain build migration can
//! tell exactly one kind of condition: a negated property, `!name`, holds,
//! since no property is set on the command line. A JDK, an OS, a file or a
//! property that must be set are the machine's or the invocation's, so a
//! profile that waits on one is listed, not merged — and while one could
//! activate, a default profile merged in gets a review line saying so.
//!
//! Merging follows Maven's profile injection, the profile dominant: properties
//! by name, dependencies and repositories by key, plugins by
//! `groupId:artifactId` with their `<configuration>` merged element by element
//! as Maven's `Xpp3Dom` does, executions by `<id>`.

use std::collections::BTreeMap;

use super::Report;
use crate::resolve::pom::{Element, Pom};

/// Every profile of `pom`, merged in when a plain build activates it, with a
/// report line each. `owner` names the POM for a parent's profiles, and is
/// `None` for the project's own.
pub(super) fn apply(pom: Pom, owner: Option<&str>, report: &mut Report) -> Pom {
    if pom.profiles.is_empty() {
        return pom;
    }
    let name = |id: &str| match owner {
        None => format!("<profile> {id}"),
        Some(owner) => format!("<profile> {id} of parent {owner}"),
    };

    let mut active = Vec::new();
    let mut by_default = Vec::new();
    // The profiles that may activate on some machine or command line, and
    // would then turn a default one off.
    let mut conditional = Vec::new();
    for profile in &pom.profiles {
        match plain_build(&profile.element) {
            Plain::Holds(why) => active.push((profile, why)),
            Plain::ByDefault => by_default.push(profile),
            Plain::Waits(condition) => {
                report.skipped(format!(
                    "{} — activated by {condition}; jrs.toml is one fixed configuration, \
                     the one a plain `mvn` build uses",
                    name(&profile.id)
                ));
                conditional.push(format!("`{}` ({condition})", profile.id));
            }
            Plain::Never => report.skipped(format!(
                "{} — active only when named with `-P {}`; jrs.toml is one fixed \
                 configuration, the one a plain `mvn` build uses",
                name(&profile.id),
                profile.id
            )),
        }
    }

    let merged: Vec<_> = if active.is_empty() {
        by_default
            .into_iter()
            .map(|p| (p, "active by default".to_string()))
            .collect()
    } else {
        let ids: Vec<String> = active.iter().map(|(p, _)| format!("`{}`", p.id)).collect();
        for profile in by_default {
            report.skipped(format!(
                "{} — active by default, but a plain `mvn` build activates {} instead, \
                 and Maven then leaves the default ones out",
                name(&profile.id),
                ids.join(", ")
            ));
        }
        active
    };
    if merged.is_empty() {
        return pom;
    }

    let mut root = pom.root;
    for (profile, why) in &merged {
        inject(&mut root, &profile.element);
        report.migrated(format!(
            "{} — {why} in a plain `mvn` build; merged in",
            name(&profile.id)
        ));
        if !conditional.is_empty() {
            report.review(format!(
                "{} was merged in, but Maven drops it whenever {} activates; \
                 jrs.toml keeps it",
                name(&profile.id),
                conditional.join(" or ")
            ));
        }
    }
    Pom::from_element(root)
}

/// What a profile does in a plain `mvn` build.
#[derive(Debug, PartialEq, Eq)]
enum Plain {
    /// Its activation holds: every condition is a negated property.
    Holds(String),
    /// No condition, but `<activeByDefault>`.
    ByDefault,
    /// A condition migration cannot decide, described.
    Waits(String),
    /// Nothing activates it but `-P`.
    Never,
}

fn plain_build(profile: &Element) -> Plain {
    let Some(activation) = profile.child("activation") else {
        return Plain::Never;
    };
    let conditions: Vec<&Element> = activation
        .children
        .iter()
        .filter(|c| c.name != "activeByDefault")
        .collect();
    let by_default = activation.text_of("activeByDefault") == Some("true");
    if conditions.is_empty() {
        return if by_default {
            Plain::ByDefault
        } else {
            Plain::Never
        };
    }
    // Since Maven 3.2.2 every condition of one activation must hold.
    let described: Vec<String> = conditions.iter().map(|c| describe(c)).collect();
    if conditions.iter().all(|c| negated_property(c)) {
        return Plain::Holds(format!("activated by {}", described.join(" and ")));
    }
    Plain::Waits(described.join(" and "))
}

/// `<property><name>!skipDocs</name></property>`: set by nothing in a plain
/// build, so the condition holds. The JVM's own properties are always set.
fn negated_property(condition: &Element) -> bool {
    const JVM_PREFIXES: &[&str] = &["java.", "os.", "user.", "file.", "line.", "path.", "sun."];
    condition.name == "property"
        && condition.text_of("value").is_none()
        && condition
            .text_of("name")
            .and_then(|n| n.strip_prefix('!'))
            .is_some_and(|n| !n.is_empty() && !JVM_PREFIXES.iter().any(|p| n.starts_with(p)))
}

/// One activation condition, as the report names it.
fn describe(condition: &Element) -> String {
    let text = |name: &str| condition.text_of(name).unwrap_or("");
    match condition.name.as_str() {
        "property" => match condition.text_of("value") {
            Some(value) => format!("property `{}={value}`", text("name")),
            None => format!("property `{}`", text("name")),
        },
        "jdk" => format!("JDK `{}`", condition.text.trim()),
        "os" => {
            let parts: Vec<String> = condition
                .children
                .iter()
                .filter(|c| !c.text.trim().is_empty())
                .map(|c| format!("{} {}", c.name, c.text.trim()))
                .collect();
            format!("OS `{}`", parts.join(", "))
        }
        "file" => {
            let parts: Vec<String> = condition
                .children
                .iter()
                .map(|c| format!("{} {}", c.name, c.text.trim()))
                .collect();
            format!("file `{}`", parts.join(", "))
        }
        other => format!("<{other}>"),
    }
}

// ---- injection -------------------------------------------------------------

/// Inject `profile` into the `<project>` element `project`, the profile
/// dominant.
fn inject(project: &mut Element, profile: &Element) {
    for section in &profile.children {
        match section.name.as_str() {
            "id" | "activation" => {}
            "properties" => merge_keyed(project, section, |e| Some(e.name.clone())),
            "dependencies" => merge_keyed(project, section, dependency_key),
            "dependencyManagement" => {
                if let Some(dependencies) = section.child("dependencies") {
                    let managed = child_mut(project, "dependencyManagement");
                    merge_keyed(managed, dependencies, dependency_key);
                }
            }
            "repositories" | "pluginRepositories" => {
                merge_keyed(project, section, |e| e.text_of("id").map(str::to_string));
            }
            "modules" => {
                let modules = child_mut(project, "modules");
                modules.children.extend(section.children.iter().cloned());
            }
            "build" => inject_build(child_mut(project, "build"), section),
            _ => replace_child(project, section.clone()),
        }
    }
}

fn inject_build(build: &mut Element, profile: &Element) {
    for section in &profile.children {
        match section.name.as_str() {
            "plugins" => merge_plugins(child_mut(build, "plugins"), section),
            "pluginManagement" => {
                if let Some(plugins) = section.child("plugins") {
                    let managed = child_mut(child_mut(build, "pluginManagement"), "plugins");
                    merge_plugins(managed, plugins);
                }
            }
            _ => replace_child(build, section.clone()),
        }
    }
}

fn merge_plugins(plugins: &mut Element, profile: &Element) {
    for plugin in &profile.children {
        let key = plugin_key(plugin);
        match plugins.children.iter_mut().find(|p| plugin_key(p) == key) {
            Some(existing) => *existing = merge_plugin(existing, plugin),
            None => plugins.children.push(plugin.clone()),
        }
    }
}

/// A plugin both declare: the profile's version, its `<configuration>`
/// merged over the project's, executions by `<id>`, dependencies by key.
fn merge_plugin(project: &Element, profile: &Element) -> Element {
    let mut out = project.clone();
    for section in &profile.children {
        match section.name.as_str() {
            "configuration" => merge_configuration(&mut out, section),
            "executions" => {
                let executions = child_mut(&mut out, "executions");
                for execution in &section.children {
                    let id = execution_id(execution);
                    match executions
                        .children
                        .iter_mut()
                        .find(|e| execution_id(e) == id)
                    {
                        Some(existing) => *existing = merge_execution(existing, execution),
                        None => executions.children.push(execution.clone()),
                    }
                }
            }
            "dependencies" => merge_keyed(&mut out, section, dependency_key),
            _ => replace_child(&mut out, section.clone()),
        }
    }
    out
}

fn merge_execution(project: &Element, profile: &Element) -> Element {
    let mut out = project.clone();
    for section in &profile.children {
        match section.name.as_str() {
            "configuration" => merge_configuration(&mut out, section),
            "goals" => {
                let goals = child_mut(&mut out, "goals");
                for goal in &section.children {
                    if !goals
                        .children
                        .iter()
                        .any(|g| g.text.trim() == goal.text.trim())
                    {
                        goals.children.push(goal.clone());
                    }
                }
            }
            _ => replace_child(&mut out, section.clone()),
        }
    }
    out
}

fn merge_configuration(owner: &mut Element, profile: &Element) {
    let merged = match owner.child("configuration") {
        Some(existing) => merge_dom(profile, existing),
        None => profile.clone(),
    };
    replace_child(owner, merged);
}

/// Maven's `Xpp3Dom` merge, `dominant` over `recessive`: the dominant value
/// wins, a child only the recessive side has is added, and children of the
/// same name are merged pairwise, in order — one the dominant side has no
/// partner for is dropped.
fn merge_dom(dominant: &Element, recessive: &Element) -> Element {
    let mut out = dominant.clone();
    if out.text.trim().is_empty() && !recessive.text.trim().is_empty() {
        out.text.clone_from(&recessive.text);
    }
    // How many of the dominant side's children of each name have a partner.
    let mut paired: BTreeMap<&str, usize> = BTreeMap::new();
    for child in &recessive.children {
        let partners: Vec<usize> = dominant
            .children
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name == child.name)
            .map(|(i, _)| i)
            .collect();
        if partners.is_empty() {
            out.children.push(child.clone());
            continue;
        }
        let used = paired.entry(child.name.as_str()).or_insert(0);
        if let Some(&at) = partners.get(*used) {
            out.children[at] = merge_dom(&out.children[at], child);
            *used += 1;
        }
    }
    out
}

// ---- element helpers -------------------------------------------------------

/// The child `name` of `parent`, added empty when there is none.
fn child_mut<'a>(parent: &'a mut Element, name: &str) -> &'a mut Element {
    let at = parent
        .children
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| {
            parent.children.push(Element {
                name: name.to_string(),
                ..Element::default()
            });
            parent.children.len() - 1
        });
    &mut parent.children[at]
}

/// Put `child` in place of `parent`'s child of the same name, or add it.
fn replace_child(parent: &mut Element, child: Element) {
    match parent.children.iter_mut().find(|c| c.name == child.name) {
        Some(existing) => *existing = child,
        None => parent.children.push(child),
    }
}

/// Merge the items of `profile` (a list element such as `<dependencies>`)
/// into `parent`'s list of the same name: an item whose key matches one there
/// replaces it, any other is added.
fn merge_keyed(parent: &mut Element, profile: &Element, key: impl Fn(&Element) -> Option<String>) {
    let list = child_mut(parent, &profile.name);
    for item in &profile.children {
        let found = key(item).and_then(|k| {
            list.children
                .iter()
                .position(|existing| key(existing).as_ref() == Some(&k))
        });
        match found {
            Some(at) => list.children[at] = item.clone(),
            None => list.children.push(item.clone()),
        }
    }
}

/// Maven's dependency key: `groupId:artifactId:type:classifier`.
fn dependency_key(dependency: &Element) -> Option<String> {
    Some(format!(
        "{}:{}:{}:{}",
        dependency.text_of("groupId")?,
        dependency.text_of("artifactId")?,
        dependency.text_of("type").unwrap_or("jar"),
        dependency.text_of("classifier").unwrap_or("")
    ))
}

fn plugin_key(plugin: &Element) -> String {
    format!(
        "{}:{}",
        plugin
            .text_of("groupId")
            .unwrap_or("org.apache.maven.plugins"),
        plugin.text_of("artifactId").unwrap_or("")
    )
}

fn execution_id(execution: &Element) -> String {
    execution.text_of("id").unwrap_or("default").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::pom::Pom;

    fn pom(body: &str) -> Pom {
        Pom::parse(
            format!(
                "<project><groupId>g</groupId><artifactId>app</artifactId>\
                 <version>1.0.0</version>{body}</project>"
            )
            .as_bytes(),
        )
        .unwrap()
    }

    fn applied(body: &str) -> (Pom, Report) {
        let mut report = Report::default();
        let pom = apply(pom(body), None, &mut report);
        (pom, report)
    }

    #[test]
    fn a_default_profile_is_merged_the_rest_are_listed() {
        let (pom, report) = applied(
            "<properties><db>h2</db></properties>\
             <profiles>\
               <profile><id>postgres</id>\
                 <activation><activeByDefault>true</activeByDefault></activation>\
                 <properties><db>postgres</db></properties>\
                 <dependencies><dependency><groupId>org.postgresql</groupId>\
                   <artifactId>postgresql</artifactId><version>42.7.3</version>\
                 </dependency></dependencies>\
               </profile>\
               <profile><id>h2</id></profile>\
               <profile><id>release</id><activation><property><name>performRelease</name>\
                 <value>true</value></property></activation></profile>\
             </profiles>",
        );
        assert_eq!(pom.properties["db"], "postgres", "the profile is dominant");
        assert_eq!(pom.dependencies.len(), 1);
        assert_eq!(pom.dependencies[0].artifact, "postgresql");

        let migrated = report.migrated.join("\n");
        assert!(
            migrated.contains("<profile> postgres — active by default in a plain `mvn` build"),
            "{migrated}"
        );
        let skipped = report.not_migrated.join("\n");
        assert!(skipped.contains("<profile> h2 — active only when named with `-P h2`"));
        assert!(
            skipped.contains("<profile> release — activated by property `performRelease=true`"),
            "{skipped}"
        );
        let review = report.needs_review.join("\n");
        assert!(
            review.contains("Maven drops it whenever `release` (property `performRelease=true`)"),
            "{review}"
        );
    }

    #[test]
    fn a_negated_property_activates_and_turns_the_defaults_off() {
        let (pom, report) = applied(
            "<profiles>\
               <profile><id>docs</id>\
                 <activation><property><name>!skipDocs</name></property></activation>\
                 <properties><docs>on</docs></properties></profile>\
               <profile><id>fallback</id>\
                 <activation><activeByDefault>true</activeByDefault></activation>\
                 <properties><fallback>on</fallback></properties></profile>\
               <profile><id>home</id>\
                 <activation><property><name>!java.home</name></property></activation>\
               </profile>\
             </profiles>",
        );
        assert_eq!(pom.properties.get("docs").map(String::as_str), Some("on"));
        assert!(!pom.properties.contains_key("fallback"));
        let migrated = report.migrated.join("\n");
        assert!(
            migrated.contains("<profile> docs — activated by property `!skipDocs`"),
            "{migrated}"
        );
        let skipped = report.not_migrated.join("\n");
        assert!(
            skipped.contains(
                "<profile> fallback — active by default, but a plain `mvn` build activates `docs`"
            ),
            "{skipped}"
        );
        assert!(
            skipped.contains("<profile> home — activated by property `!java.home`"),
            "the JVM's own properties are always set: {skipped}"
        );
    }

    #[test]
    fn plugins_merge_by_key_with_their_configuration_merged_element_by_element() {
        let (pom, _) = applied(
            "<build><plugins><plugin><artifactId>maven-surefire-plugin</artifactId>\
               <configuration><forkCount>2</forkCount><argLine>-Xmx1g</argLine>\
                 <systemPropertyVariables><a>1</a></systemPropertyVariables>\
               </configuration>\
               <executions><execution><id>it</id><goals><goal>test</goal></goals>\
               </execution></executions>\
             </plugin></plugins></build>\
             <profiles><profile><id>ci</id>\
               <activation><activeByDefault>true</activeByDefault></activation>\
               <build><plugins>\
                 <plugin><groupId>org.apache.maven.plugins</groupId>\
                   <artifactId>maven-surefire-plugin</artifactId>\
                   <configuration><argLine>-Xmx2g</argLine>\
                     <systemPropertyVariables><b>2</b></systemPropertyVariables>\
                   </configuration>\
                   <executions><execution><id>it</id><goals><goal>verify</goal></goals>\
                   </execution></executions>\
                 </plugin>\
                 <plugin><artifactId>maven-antrun-plugin</artifactId></plugin>\
               </plugins></build>\
             </profile></profiles>",
        );
        let plugins = &pom.build.plugins;
        assert_eq!(plugins.len(), 2, "{plugins:?}");
        let config = plugins[0].configuration.as_ref().unwrap();
        assert_eq!(
            config.text_of("forkCount"),
            Some("2"),
            "kept from the project"
        );
        assert_eq!(
            config.text_of("argLine"),
            Some("-Xmx2g"),
            "the profile's wins"
        );
        let properties: Vec<&str> = config
            .child("systemPropertyVariables")
            .unwrap()
            .children
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(properties, ["b", "a"], "merged, the profile's first");
        let goals: Vec<&str> = plugins[0].executions[0]
            .child("goals")
            .unwrap()
            .children
            .iter()
            .map(|g| g.text.trim())
            .collect();
        assert_eq!(goals, ["test", "verify"]);
        assert_eq!(plugins[1].artifact, "maven-antrun-plugin");
    }

    #[test]
    fn the_dom_merge_pairs_same_named_children_in_order() {
        let parse = |xml: &str| crate::resolve::pom::parse_xml(xml.as_bytes()).unwrap();
        let merged = merge_dom(
            &parse("<c><arg>-a</arg></c>"),
            &parse("<c><arg>-x</arg><arg>-y</arg><only>1</only></c>"),
        );
        let children: Vec<(&str, &str)> = merged
            .children
            .iter()
            .map(|c| (c.name.as_str(), c.text.trim()))
            .collect();
        assert_eq!(
            children,
            [("arg", "-a"), ("only", "1")],
            "the second recessive <arg> has no partner and is dropped, as in Maven"
        );
    }

    #[test]
    fn dependencies_and_repositories_merge_by_key() {
        let (pom, _) = applied(
            "<dependencies><dependency><groupId>g</groupId><artifactId>a</artifactId>\
               <version>1</version></dependency></dependencies>\
             <repositories><repository><id>corp</id><url>https://a</url></repository>\
             </repositories>\
             <profiles><profile><id>p</id>\
               <activation><activeByDefault>true</activeByDefault></activation>\
               <dependencies><dependency><groupId>g</groupId><artifactId>a</artifactId>\
                 <version>2</version></dependency></dependencies>\
               <repositories><repository><id>corp</id><url>https://b</url></repository>\
               </repositories>\
             </profile></profiles>",
        );
        assert_eq!(pom.dependencies.len(), 1);
        assert_eq!(pom.dependencies[0].version.as_deref(), Some("2"));
        assert_eq!(pom.repositories, [("corp".into(), "https://b".into())]);
    }

    #[test]
    fn conditions_are_described_and_all_must_hold() {
        let profile = |activation: &str| {
            crate::resolve::pom::parse_xml(
                format!("<profile><activation>{activation}</activation></profile>").as_bytes(),
            )
            .unwrap()
        };
        assert_eq!(
            plain_build(&profile("<jdk>[21,)</jdk>")),
            Plain::Waits("JDK `[21,)`".into())
        );
        assert_eq!(
            plain_build(&profile("<os><family>windows</family></os>")),
            Plain::Waits("OS `family windows`".into())
        );
        assert_eq!(
            plain_build(&profile(
                "<property><name>!skip</name></property><file><exists>x</exists></file>"
            )),
            Plain::Waits("property `!skip` and file `exists x`".into()),
            "a negated property alone would hold; with a file it waits"
        );
        assert_eq!(
            plain_build(&profile("<activeByDefault>false</activeByDefault>")),
            Plain::Never
        );
    }
}
