use anyhow::Result as AnyResult;
use regex::Regex;
use std::ffi::{OsStr, OsString};
use std::fmt::Debug;
use std::fs::DirEntry;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

lazy_static! {
    static ref NUMBER: Regex = Regex::new(r"\d+").unwrap();
}

static EXTENSIONS: &[&str] = &[
    "cdg", "idx", "srt", "sub", "utf", "ass", "ssa", "aqt", "jss", "psb", "rt", "sami", "smi",
    "smil", "stl", "usf", "dks", "pjs", "mpl2", "mks", "vtt", "tt", "ttml", "dfxp", "scc", "itt",
    "sbv", "aaf", "mcc", "mxf", "asc", "cap", "onl", "cin", "ult", "scr", "sst", "nav", "son",
];

#[derive(Debug)]
pub struct SubAndVid {
    pub sub_path: PathBuf,
    pub sub_file_part: String, // lossy if not UTF-8
    pub sub_ext_part: OsString,
    pub vid_path: PathBuf,
    pub vid_file_part: String, // lossy if not UTF-8
    pub vid_ext_part: Option<OsString>,
}

#[derive(Debug)]
pub struct MatchInfo {
    pub matched: SubAndVid,
    // These ranges are indices in to the sub and file file path strings
    sub_match_range: Range<usize>,
    vid_match_range: Range<usize>,
}

impl MatchInfo {
    pub fn sub_match_parts(&self) -> (&str, &str, &str) {
        let before = &self.matched.sub_file_part[0..self.sub_match_range.start];
        let matched = &self.matched.sub_file_part[self.sub_match_range.clone()];
        let after = &self.matched.sub_file_part[self.sub_match_range.end..];
        (before, matched, after)
    }

    pub fn vid_match_parts(&self) -> (&str, &str, &str) {
        let before = &self.matched.vid_file_part[0..self.vid_match_range.start];
        let matched = &self.matched.vid_file_part[self.vid_match_range.clone()];
        let after = &self.matched.vid_file_part[self.vid_match_range.end..];
        (before, matched, after)
    }
}

impl SubAndVid {
    fn new(sub_path: impl Into<PathBuf>, vid_path: impl Into<PathBuf>) -> SubAndVid {
        let sub_path = sub_path.into();
        let vid_path = vid_path.into();

        let (sub_file_part, sub_ext_part) =
            split_extension(&sub_path).expect("sub file didn't have an extension");

        let (vid_file_part, vid_ext_part) = split_extension(&vid_path)
            .map(|(f, e)| (f, Some(e)))
            .unwrap_or((vid_path.as_os_str(), None));

        let sub_file_part = sub_file_part.to_owned().to_string_lossy().to_string();
        let sub_ext_part = sub_ext_part.to_owned();

        let vid_file_part = vid_file_part.to_owned().to_string_lossy().to_string();
        let vid_ext_part = vid_ext_part.map(OsStr::to_owned);

        SubAndVid {
            sub_path,
            sub_file_part,
            sub_ext_part,
            vid_path,
            vid_file_part,
            vid_ext_part,
        }
    }
}

pub struct ScanOptions<'a> {
    pub path: &'a Path,
    pub sub_area: Option<&'a Regex>,
    pub video_area: Option<&'a Regex>,
    pub sub_filter: Option<&'a Regex>,
    pub video_filter: Option<&'a Regex>,
}

pub fn scan(options: ScanOptions) -> AnyResult<Vec<MatchInfo>> {
    let files_with_numbers = scan_number_files(&options)?;
    let matched = match_files(&options, &files_with_numbers)?;
    Ok(matched)
}

fn scan_number_files(options: &ScanOptions) -> AnyResult<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = entries(options.path)?
        .iter()
        .map(|e| e.path())
        .filter(|p| p.is_file() && NUMBER.is_match(&p.to_string_lossy()))
        .collect();

    files.sort();

    Ok(files)
}

pub fn scan_subs_only(options: ScanOptions) -> AnyResult<Vec<PathBuf>> {
    let subs = entries(options.path)?
        .into_iter()
        .map(|e| e.path())
        .filter(|p| {
            let ext = p.extension().and_then(OsStr::to_str).unwrap_or_default();
            p.is_file() && EXTENSIONS.contains(&ext)
        })
        .filter(|sub| regex_matches_file_name(options.sub_filter, sub))
        .collect();

    Ok(subs)
}

fn entries(path: &Path) -> io::Result<Vec<DirEntry>> {
    std::fs::read_dir(path)?.collect::<io::Result<Vec<DirEntry>>>()
}

fn match_files(options: &ScanOptions, files_with_numbers: &[PathBuf]) -> AnyResult<Vec<MatchInfo>> {
    // Separate subtitle files from non-subtitle files.
    let (subs, others): (Vec<&PathBuf>, Vec<&PathBuf>) =
        files_with_numbers.iter().partition(|file| {
            let ext = file.extension().and_then(OsStr::to_str).unwrap_or_default();
            EXTENSIONS.contains(&ext)
        });

    // Remove files that don't match the filters.
    let mut subs = subs
        .into_iter()
        .filter(|sub| regex_matches_file_name(options.sub_filter, sub))
        .collect();
    let mut others = others
        .into_iter()
        .filter(|other| regex_matches_file_name(options.video_filter, other))
        .collect();

    // Find subs that already match their video files and return and remove them from subs
    // and others.
    let same = extract_same(&mut subs, &mut others);

    // Find the areas inside the paths that match the area regular expressions.
    let sub_areas = find_areas(subs, &options.sub_area)?;
    let mut other_areas = find_areas(others, &options.video_area)?;

    // Match the subtitle and other paths where they have the same number in their areas.
    let mut matched: Vec<MatchInfo> = sub_areas
        .iter()
        .filter_map(|sub| {
            let num_range = NUMBER.find(&sub.area).map(|m| m.range())?;
            let num = sub.area[num_range.clone()]
                .parse::<u32>()
                .unwrap()
                .to_string(); // remove leading zeroes

            let (other_index, other, position) = other_areas
                .iter()
                .enumerate()
                .find(|(_, other)| other.area.contains(&num))
                .map(|(index, other)| (index, other, other.area.find(&num).unwrap()))?;

            let matched = Some(MatchInfo {
                matched: SubAndVid::new(sub.path, other.path),

                sub_match_range: (sub.area_start_index + num_range.start)
                    ..(sub.area_start_index + num_range.end),

                vid_match_range: (other.area_start_index + position)
                    ..(other.area_start_index + position + num.len()),
            });

            other_areas.remove(other_index);
            matched
        })
        .collect();

    matched.extend(same);
    Ok(matched)
}

fn regex_matches_file_name(regex: Option<&Regex>, path: &Path) -> bool {
    match regex {
        Some(regex) => match try_extract_file_name(path) {
            Ok(file_name) => regex.is_match(&file_name),
            _ => false,
        },
        None => true,
    }
}

fn extract_same(subs: &mut Vec<&PathBuf>, others: &mut Vec<&PathBuf>) -> Vec<MatchInfo> {
    let mut same = Vec::new();

    subs.retain(|sub| {
        let (sub_file_part, _) = split_extension(sub).expect("sub file didn't have an extension");

        if let Some((index, other_file_part)) = others
            .iter()
            .map(|other| {
                // We may not have an extension, if not return the entire path
                split_extension(other)
                    .map(|(other_file_part, _)| other_file_part)
                    .unwrap_or(other.file_stem().unwrap_or(OsStr::new("")))
            })
            .enumerate()
            .find(|(_, other_file_part)| {
                sub_file_part == *other_file_part
            })
        {
            let other = others.remove(index);
            same.push(MatchInfo {
                matched: SubAndVid::new(sub, other),
                sub_match_range: 0..sub_file_part.to_string_lossy().len(),
                vid_match_range: 0..other_file_part.to_string_lossy().len(),
            });
            false
        } else {
            true
        }
    });
    same
}

fn split_extension(path: &Path) -> Option<(&OsStr, &OsStr)> {
    Some((path.file_stem()?, path.extension()?))
}

fn find_areas<'a>(
    paths: Vec<&'a PathBuf>,
    area_matcher: &Option<&Regex>,
) -> AnyResult<Vec<PathAndArea<'a>>> {
    paths
        .iter()
        .map(|path| -> AnyResult<PathAndArea> {
            let (area, area_start_index) = try_extract_area(path, area_matcher)?;
            Ok(PathAndArea {
                path,
                area,
                area_start_index,
            })
        })
        .collect::<AnyResult<_>>()
}

fn try_extract_area(path: &Path, regex: &Option<&Regex>) -> AnyResult<(String, usize)> {
    let name = try_extract_file_name(path)?;

    if let Some(r) = regex {
        if let Some(m) = r.find(&name) {
            Ok((m.as_str().into(), m.start()))
        } else {
            bail!("failed to match regex {} on text: {}", r, name);
        }
    } else {
        Ok((name, 0))
    }
}

fn try_extract_file_name(path: &Path) -> AnyResult<String> {
    Ok(path
        .file_name()
        .ok_or_else(|| anyhow!("file {} has an invalid file name", path.to_string_lossy()))?
        .to_string_lossy()
        .to_string())
}

#[derive(Debug)]
struct PathAndArea<'a> {
    path: &'a Path,
    area: String,
    // Where the area starts in path
    area_start_index: usize,
}
