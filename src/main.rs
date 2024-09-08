#![feature(path_add_extension)]

use std::{
    cmp::Reverse,
    collections::HashMap,
    fs::{self, create_dir_all, File},
    io::{BufReader, BufWriter, Write},
    iter,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use clap::Parser;
use exif::{In, Tag, Value};
use inotify::{Inotify, WatchMask};
use itertools::Itertools as _;

#[derive(Parser)]
struct Args {
    input_dir: Option<String>,

    #[arg(short, long)]
    output_dir: Option<String>,

    #[arg(short, long)]
    watch: bool,
}

#[derive(Debug)]
struct Options {
    input_dir: PathBuf,
    output_dir: PathBuf,
    thumbnail_dir: PathBuf,
    img_dir: PathBuf,
}

impl From<Args> for Options {
    fn from(value: Args) -> Self {
        let input_dir = PathBuf::from(&value.input_dir.unwrap_or_else(|| "./".to_owned()));
        let output_dir = if let Some(output_dir) = value.output_dir {
            PathBuf::from(output_dir)
        } else {
            input_dir.parent().unwrap().join("web").to_owned()
        };
        let thumbnail_dir = output_dir.join("thumbnail");
        let img_dir = output_dir.join("img");
        for d in [&thumbnail_dir, &img_dir] {
            if !d.exists() {
                create_dir_all(&d).unwrap();
            }
        }
        Self {
            input_dir,
            output_dir,
            thumbnail_dir,
            img_dir,
        }
    }
}

impl Options {
    fn relative_path<'a>(&self, path: &'a Path) -> &'a Path {
        dbg!(path);
        dbg!(&self.output_dir);
        path.strip_prefix(&self.output_dir).unwrap()
    }
}

#[derive(Debug)]
struct Photo {
    original_path: PathBuf,
    datetime: NaiveDateTime,
    thumbnail_path: PathBuf,
    img_path: PathBuf,
}

impl Photo {
    fn new(path: PathBuf, options: &Options) -> Self {
        let file = File::open(&path).unwrap();
        let mut buf_reader = BufReader::new(file);
        let exif_reader = exif::Reader::new();
        let exif = exif_reader.read_from_container(&mut buf_reader).unwrap();
        let datetime = &exif
            .get_field(Tag::DateTimeOriginal, In::PRIMARY)
            .unwrap()
            .value;
        let offset = &exif
            .get_field(Tag::OffsetTimeOriginal, In::PRIMARY)
            .unwrap()
            .value;
        let datetime =
            NaiveDateTime::parse_from_str(&ascii_to_string(datetime), "%Y:%m:%d %H:%M:%S").unwrap();
        let offset = ascii_to_string(offset).parse::<FixedOffset>().unwrap();
        let datetime = offset.from_local_datetime(&datetime).unwrap().naive_local();
        let thumbnail_path = Self::generate_image::<true>(&path, options);
        let img_path = Self::generate_image::<false>(&path, options);

        return Self {
            original_path: path,
            datetime,
            thumbnail_path,
            img_path,
        };

        fn ascii_to_string(v: &Value) -> String {
            if let Value::Ascii(date) = v {
                let s: Vec<u8> = date.iter().flatten().map(|c| *c).collect();
                String::from_utf8(s).unwrap()
            } else {
                panic!()
            }
        }
    }

    fn generate_image<const THUMBNAIL: bool>(input: &Path, options: &Options) -> PathBuf {
        let filename = input.file_name().unwrap();
        let output_path = if THUMBNAIL {
            &options.thumbnail_dir
        } else {
            &options.img_dir
        }
        .join(filename)
        .with_extension("jpg");
        if output_path.exists() {
            let generate_time = output_path.metadata().unwrap().modified().unwrap();
            let photo_time = input.metadata().unwrap().modified().unwrap();
            if generate_time > photo_time {
                return output_path;
            }
        }
        let mut command = Command::new("magick");
        command.arg(input.as_os_str()).arg("-strip");
        if THUMBNAIL {
            command.arg("-quality").arg("65%").arg("-resize").arg("512");
        }
        command
            .arg("-sampling-factor")
            .arg("4:2:0")
            .arg(output_path.as_os_str());
        dbg!(&command);
        let status = command.status().unwrap();
        assert!(status.success());
        output_path
    }
}

fn generate(options: &Options) {
    let entries = fs::read_dir(&options.input_dir).unwrap();

    let photos: Vec<Photo> = entries
        .map(|e| {
            let path = e.unwrap().path();
            Photo::new(path, &options)
        })
        .collect();
    dbg!(&photos);

    let mut photos_by_day: HashMap<NaiveDate, Vec<Photo>> = HashMap::new();

    for p in photos {
        let date = p.datetime.date();
        photos_by_day.entry(date).or_insert(Vec::new()).push(p);
    }

    for v in photos_by_day.values_mut() {
        v.sort_by_key(|p| Reverse(p.datetime))
    }

    dbg!(&photos_by_day);

    let mut photos_by_day: Vec<_> = photos_by_day.into_iter().collect();
    photos_by_day.sort_by_key(|k| Reverse(k.0));

    dbg!(&photos_by_day);

    let mut page_num_photo = 0;
    const MAX_NUM_PHOTO_PER_PAGE: usize = 50;
    let pages: Vec<&[(NaiveDate, Vec<Photo>)]> = photos_by_day.split_inclusive(|(_, v)| {
        page_num_photo += v.len();
        if page_num_photo > MAX_NUM_PHOTO_PER_PAGE {
            page_num_photo = v.len();
            true
        } else {
            false
        }
    }).collect();

    assert_eq!(pages.iter().map(|s| s.len()).sum::<usize>(), photos_by_day.len());

    dbg!(&pages);

    let nav: String = iter::once("<hr>\n<ul class=\"nav\">\n".to_owned())
        .chain(pages.iter().enumerate().map(|(index, page)| {
            let (start_date, _) = page.last().unwrap();
            let (end_date, _) = page.first().unwrap();
            let text = if start_date < end_date {
                format!("{:?}–{:?}", start_date, end_date)
            } else {
                assert!(start_date == end_date);
                format!("{:?}", start_date)
            };
            let path = page_path(index);
            format!("<li><a href=\"{path}\" class=\"page_{index}\">{text}</a></li>\n")
        }))
        .chain(iter::once("</ul>\n".to_owned()))
        .collect();

    for (index, photos_by_day) in pages.iter().enumerate() {
        generate_page(photos_by_day, options, index, &nav);
    }
}

fn page_path(index: usize) -> String {
    format!("page_{index}.html")
}

fn generate_page(
    photos_by_day: &[(NaiveDate, Vec<Photo>)],
    options: &Options,
    index: usize,
    nav: &str,
) {
    let path = page_path(index);
    let style = format!(
        "<style>
a.page_{index} {{
    font-weight: bold;
    color: gray;
}}
</style>
"
    );
    let body: Vec<_> = photos_by_day
        .iter()
        .map(|(date, v)| {
            (
                date,
                iter::once(format!(
                    "<h2>{:?}</h2>\n<div class=\"masonry-grid\">\n",
                    date
                ))
                .chain(v.iter().map(|p| {
                    format!(
                        "<figure><a href=\"{}\"><img src=\"./{}\"></figure></a>\n",
                        options.relative_path(&p.img_path).to_str().unwrap(),
                        options.relative_path(&p.thumbnail_path).to_str().unwrap()
                    )
                }))
                .chain(iter::once(format!("</div>\n"))),
            )
        })
        .collect();

    let body: Vec<String> = body.into_iter().map(|(_, i)| i).flatten().collect();

    let html = [HTML_BEGIN, style.as_str(), "<body>\n"]
        .into_iter()
        .chain(body.iter().map(|s| &**s))
        .chain(["</body>", nav, HTML_END].into_iter());

    let index_path = options.output_dir.join(path);
    let mut writer = BufWriter::new(File::create(index_path).unwrap());

    for s in html {
        writer.write_all(s.as_bytes()).unwrap();
    }
}

fn main() {
    let args = Args::parse();
    let watch = args.watch;
    let options: Options = args.into();
    dbg!(&options);
    generate(&options);
    if !watch {
        return;
    }
    watch_and_generate(&options);
}

fn watch_and_generate(options: &Options) {
    let mut inotify = Inotify::init().unwrap();
    inotify
        .watches()
        .add(
            &options.input_dir,
            WatchMask::MODIFY | WatchMask::CREATE | WatchMask::DELETE,
        )
        .unwrap();
    dbg!("Watching", &options.input_dir);
    let mut buffer = [0u8; 4096];
    loop {
        let events = inotify.read_events_blocking(&mut buffer).unwrap();
        for e in events {
            dbg!(e);
        }
        generate(options);
    }
}

const HTML_BEGIN: &'static str = r##"
<!DOCTYPE html>
<html lang="en">

<head>
    <meta charset="utf-8">
    <title>Photos</title>
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <link rel="stylesheet" type="text/css" href="./css/style.css">
    <link rel="icon" href="/favicon.ico" sizes="any">
    <link rel="icon" href="/icon.svg" type="image/svg+xml">
    <link rel="apple-touch-icon" href="/apple-touch-icon.png">
    <link rel="manifest" href="/site.webmanifest">
    <meta name="theme-color" content="#ffffff">
</head>

"##;

const HTML_END: &'static str = r##"


</html>

"##;
