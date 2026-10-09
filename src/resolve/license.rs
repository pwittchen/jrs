//! Licences: what each dependency's POM says it is published under, for
//! `jrs licenses` and the SBOM `jrs package --sbom` writes.
//!
//! A POM's `<licenses>` is inherited: a POM that declares none takes its
//! parent's, as Maven's own model does, so the chain is walked until one
//! does. Nothing here is checked or interpreted beyond recognising the common
//! licences' SPDX identifiers; a dependency that declares nothing is reported
//! as such, never guessed at.

use rayon::prelude::*;

use super::coord::Coord;
use super::pom::{Element, Pom};
use super::repo::Fetcher;
use crate::error::{JrsError, Result};

/// One `<license>` of a POM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct License {
    pub name: Option<String>,
    pub url: Option<String>,
    /// The SPDX identifier, when the name or the URL is a licence jrs knows.
    pub spdx: Option<&'static str>,
}

impl License {
    /// The SPDX identifier when there is one, otherwise the name, otherwise
    /// the URL.
    #[must_use]
    pub fn label(&self) -> String {
        self.spdx
            .map(str::to_string)
            .or_else(|| self.name.clone())
            .or_else(|| self.url.clone())
            .unwrap_or_default()
    }
}

/// The `<licenses>` `root` declares itself; empty when it declares none.
#[must_use]
pub fn declared(root: &Element) -> Vec<License> {
    root.list("licenses", "license")
        .into_iter()
        .filter_map(|l| {
            let text = |name: &str| {
                l.text_of(name)
                    .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|t| !t.is_empty())
            };
            let (name, url) = (text("name"), text("url"));
            if name.is_none() && url.is_none() {
                return None;
            }
            let spdx = spdx_id(name.as_deref(), url.as_deref());
            Some(License { name, url, spdx })
        })
        .collect()
}

/// The licences `coord`'s POM declares, or the nearest parent's that does.
///
/// # Errors
///
/// [`JrsError::Resolve`] when a POM in the chain cannot be fetched or read.
pub fn of(fetcher: &Fetcher, coord: &Coord) -> Result<Vec<License>> {
    let mut current = coord.pom_coord();
    for _ in 0..32 {
        let bytes = fetcher.pom(&current)?;
        let pom = Pom::parse(&bytes).map_err(|e| JrsError::resolve(format!("{current}: {e}")))?;
        let licenses = declared(&pom.root);
        if !licenses.is_empty() {
            return Ok(licenses);
        }
        match pom.parent {
            Some(p) => current = Coord::new(&p.group, &p.artifact, &p.version),
            None => break,
        }
    }
    Ok(Vec::new())
}

/// [`of`] for each coordinate, `jobs` at a time, in the order given.
#[must_use]
pub fn of_each(fetcher: &Fetcher, coords: &[Coord], jobs: usize) -> Vec<Result<Vec<License>>> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build();
    match pool {
        Ok(pool) => pool.install(|| coords.par_iter().map(|c| of(fetcher, c)).collect()),
        Err(_) => coords.iter().map(|c| of(fetcher, c)).collect(),
    }
}

/// The SPDX identifiers jrs recognises by themselves, as a name.
const SPDX_IDS: &[&str] = &[
    "0BSD",
    "Apache-1.1",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "CDDL-1.0",
    "CDDL-1.1",
    "EPL-1.0",
    "EPL-2.0",
    "GPL-2.0-only",
    "GPL-3.0-only",
    "ISC",
    "LGPL-2.1-only",
    "LGPL-3.0-only",
    "MIT",
    "MIT-0",
    "MPL-1.1",
    "MPL-2.0",
    "Unlicense",
    "Zlib",
];

/// The SPDX identifier of a licence by the name or URL a POM gives it, for
/// the spellings the most common licences go by in Maven Central. `None`
/// for anything less clear-cut: a wrong identifier in an SBOM is worse than
/// a name.
#[must_use]
pub fn spdx_id(name: Option<&str>, url: Option<&str>) -> Option<&'static str> {
    if let Some(name) = name
        && let Some(id) = SPDX_IDS
            .iter()
            .find(|id| id.eq_ignore_ascii_case(name.trim()))
    {
        return Some(id);
    }
    let name = name.unwrap_or_default().to_ascii_lowercase();
    let url = url
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace("https://", "")
        .replace("http://", "")
        .replace("www.", "");
    let has = |words: &[&str]| words.iter().all(|w| name.contains(w));
    let at = |prefixes: &[&str]| prefixes.iter().any(|p| url.starts_with(p));

    if (has(&["apache"]) && (has(&["2.0"]) || has(&["2 "]) || name.ends_with(" 2") || has(&["v2"])))
        || at(&[
            "apache.org/licenses/license-2.0",
            "opensource.org/licenses/apache-2.0",
        ])
    {
        return Some("Apache-2.0");
    }
    if has(&["eclipse public license"]) || has(&["epl"]) {
        if has(&["2.0"]) || has(&["v2"]) {
            return Some("EPL-2.0");
        }
        if has(&["1.0"]) || has(&["v1"]) || has(&["v 1"]) {
            return Some("EPL-1.0");
        }
    }
    if at(&[
        "eclipse.org/legal/epl-2.0",
        "opensource.org/licenses/epl-2.0",
    ]) {
        return Some("EPL-2.0");
    }
    if at(&[
        "eclipse.org/legal/epl-v10",
        "opensource.org/licenses/epl-1.0",
    ]) {
        return Some("EPL-1.0");
    }
    if matches!(
        name.as_str(),
        "mit" | "mit license" | "the mit license" | "the mit license (mit)" | "mit-license"
    ) || at(&["opensource.org/licenses/mit", "mit-license.org"])
    {
        return Some("MIT");
    }
    if has(&["bsd"])
        && (has(&["3-clause"]) || has(&["3 clause"]) || has(&["new"]) || has(&["revised"]))
        || at(&["opensource.org/licenses/bsd-3-clause"])
    {
        return Some("BSD-3-Clause");
    }
    if has(&["bsd"]) && (has(&["2-clause"]) || has(&["2 clause"]) || has(&["simplified"]))
        || at(&["opensource.org/licenses/bsd-2-clause"])
    {
        return Some("BSD-2-Clause");
    }
    if (has(&["mozilla public license"]) && has(&["2.0"]))
        || at(&["mozilla.org/mpl/2.0", "opensource.org/licenses/mpl-2.0"])
    {
        return Some("MPL-2.0");
    }
    if has(&["cc0"]) || at(&["creativecommons.org/publicdomain/zero/1.0"]) {
        return Some("CC0-1.0");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::pom::parse_xml;

    #[test]
    fn a_poms_licences_are_read_with_their_spdx_ids() {
        let root = parse_xml(
            b"<project><licenses>\
              <license><name>The Apache Software License, Version 2.0</name>\
              <url>https://www.apache.org/licenses/LICENSE-2.0.txt</url></license>\
              <license><name>Some\n   Custom Licence</name></license>\
              <license><comments>nothing to go by</comments></license>\
              </licenses></project>",
        )
        .unwrap();
        let licenses = declared(&root);
        assert_eq!(
            licenses.len(),
            2,
            "a licence with no name and no URL is none"
        );
        assert_eq!(licenses[0].spdx, Some("Apache-2.0"));
        assert_eq!(licenses[0].label(), "Apache-2.0");
        assert_eq!(licenses[1].spdx, None);
        assert_eq!(licenses[1].label(), "Some Custom Licence");
        assert!(declared(&parse_xml(b"<project/>").unwrap()).is_empty());
    }

    #[test]
    fn common_licences_are_recognised_by_name_or_url() {
        for (name, url, id) in [
            (Some("Apache License, Version 2.0"), None, "Apache-2.0"),
            (Some("Apache-2.0"), None, "Apache-2.0"),
            (
                None,
                Some("http://www.apache.org/licenses/LICENSE-2.0"),
                "Apache-2.0",
            ),
            (Some("MIT License"), None, "MIT"),
            (
                Some("The MIT License"),
                Some("https://opensource.org/licenses/MIT"),
                "MIT",
            ),
            (Some("Eclipse Public License v2.0"), None, "EPL-2.0"),
            (Some("Eclipse Public License - v 1.0"), None, "EPL-1.0"),
            (
                None,
                Some("https://www.eclipse.org/legal/epl-2.0/"),
                "EPL-2.0",
            ),
            (Some("BSD-3-Clause"), None, "BSD-3-Clause"),
            (Some("New BSD License"), None, "BSD-3-Clause"),
            (Some("The BSD 2-Clause License"), None, "BSD-2-Clause"),
            (Some("Mozilla Public License, Version 2.0"), None, "MPL-2.0"),
            (Some("CC0"), None, "CC0-1.0"),
        ] {
            assert_eq!(spdx_id(name, url), Some(id), "{name:?} {url:?}");
        }
        for (name, url) in [
            (Some("GNU Lesser General Public License"), None),
            (Some("CDDL + GPLv2 with classpath exception"), None),
            (Some("BSD"), None),
            (None, None),
        ] {
            assert_eq!(spdx_id(name, url), None, "{name:?}: not clear-cut");
        }
    }
}
