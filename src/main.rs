extern crate failure;
#[macro_use] extern crate failure_derive;
extern crate itertools;
extern crate rayon;
extern crate reqwest;
extern crate select;
extern crate serenity;
extern crate typemap;
extern crate tantivy;
#[macro_use] extern crate log;
extern crate flexi_logger;

use std::{ fmt::Write, fs, thread };

use failure::{ Error, SyncFailure };
use itertools::Itertools;
use serenity::{
    prelude::*,
    model::{
        channel::Message,
        gateway::Game,
        guild::Member,
        id::ChannelId
    },
};

static IMAGE_DATA: &[u8] = include_bytes!("../uw_logo.png");
const EMBED_COLOR: u32 = 0x00005696;

mod uwin;

fn main() {
    // Setup logger.
    flexi_logger::Logger::with_str("uwinsearch")
        .start()
        .expect("Couldn't initialize logger.");

    info!("Initializing course index...");

    let index = uwin::CourseIndex::open()
        .expect("Couldn't open index and courses.");

    info!("Starting Discord bot...");

    let token = env!("DISCORD_TOKEN");

    let mut client = Client::new(token, Handler)
        .expect("Error creating discord client.");

    client.data.lock().insert::<uwin::CourseIndex>(index);

    if let Err(e) = client.start() {
        error!("Error running Discord bot: {}", e);
    }
}

struct Handler;

impl Handler {
    fn fetch_course(&self, ctx: Context, q: &str, chan: ChannelId) -> Result<(), Error> {
        if q.is_empty() {
            chan.send_message(|m| {
                m.embed(|e| {
                    e.color(EMBED_COLOR)
                        .field("Usage", "~course <query>", false)
                        .field("Examples", "~course 60100\n~course calculus", false)
                })
            }).map_err(SyncFailure::new)?;

            return Ok(())
        }

        // Make the bot seem like it's typing a message just in case the query
        // takes longer than expected
        chan.broadcast_typing()
            .map_err(SyncFailure::new)?;

        // Search the index and get the information from the cache.

        let data = ctx.data.lock();

        // We'll only index if the course index exists. It can be missing if
        // we are reindexing courses.
        let index = if let Some(index) = data.get::<uwin::CourseIndex>() {
            index
        } else {
            return Ok(());
        };

        // TODO: Support selecting terms/semesters
        let mut courses = index.query("20185", q)?;

        // Sort the courses in order by code.
        courses.sort_by(|c, other| c.code.cmp(&other.code));

        match courses.as_slice() {
            [] => {
                chan.send_message(|m| {
                        m.content(format!("No course found for query `{}`.", q))
                    })
                    .map_err(SyncFailure::new)?;
            }
            [course] => {
                let course = course.scrape()?;

                let title = course.title;

                let description = course.description
                    .chars()
                    .take(200)
                    .chain("...\n\n".chars())
                    .collect::<String>();

                let mut fields = Vec::new();

                if let Some(note) = course.note {
                    fields.push(("Note", note, false));
                }

                fields.push(("Meets", course.meets, false));

                if course.instructors.len() > 0 {
                    let instructors = course.instructors
                        .iter()
                        .fold(String::new(), |mut s, ins| {
                            if let Some(url) = ins.directory_url() {
                                let _ = writeln!(s, "[{}]({})", ins.name, url);
                            } else {
                                let _ = writeln!(s, "{}", ins.name);
                            }

                            s
                        });

                    fields.push(("Instructors", instructors, true));
                }

                fields.push(("Availability", course.availability, true));

                if course.prereqs.len() > 0 {
                    let prereqs = course.prereqs
                        .iter()
                        .join("\n");

                    fields.push(("Prerequisites", prereqs, false));
                }

                if course.exams.len() > 0 {
                    let exams = course.exams
                        .iter()
                        .fold(String::new(), |mut s, ex| {
                            let _ = write!(s, "**{}**", ex.ty);

                            if let Some(date) = ex.date.as_ref() {
                                let _ = write!(s, " on {}", date);
                            }

                            if let Some(time) = ex.time.as_ref() {
                                let _ = write!(s, " at {}", time);
                            }

                            if let Some(building) = ex.building.as_ref() {
                                let _ = write!(s, " in {}", building);
                            }

                            if let Some(room) = ex.room.as_ref() {
                                let _ = write!(s, " room {}", room);
                            }

                            let _ = writeln!(s);

                            s
                        });

                    fields.push(("Exams", exams, false));
                }

                let files = vec![(IMAGE_DATA, "icon.png")];
                chan.send_files(files, |m| {
                        m.embed(|e| {
                            e.color(EMBED_COLOR)
                                .thumbnail("attachment://icon.png")
                                .title(title)
                                .description(description)
                                .fields(fields)
                        })
                    })
                    .map_err(SyncFailure::new)?;
            }
            courses => {
                let (codes, titles) = courses.iter()
                    .fold((String::new(), String::new()), |mut s, course| {
                        let _ = writeln!(s.0, "{}", &course.code);
                        let _ = writeln!(s.1, "{}", &course.title);
                        s
                    });

                chan.send_message(|m| {
                        m.embed(|e| {
                            e.color(EMBED_COLOR)
                                .field("Course Code", codes, true)
                                .field("Titles", titles, true)
                        })
                    })
                    .map_err(SyncFailure::new)?;
            }
        }

        Ok(())
    }

    fn reindex(&self, ctx: Context, member: Option<Member>) -> Result<(), Error> {
        // We want to reindex if a person from a channel is an administrator.
        if let Some(member) = member {
            let is_admin = member.permissions()
                .map(|perm| perm.administrator())
                .unwrap_or(false);

            if is_admin {
                // Remove current course index.
                let mut data = ctx.data.lock();

                let already_reindexing = data.remove::<uwin::CourseIndex>()
                    .is_none();

                if already_reindexing {
                    return Ok(());
                }

                // Rebuild course index in another thread.
                let shard = ctx.shard;
                let data = ctx.data.clone();
                thread::spawn(move || {
                    shard.set_game(Some(Game::playing("Reindexing...")));

                    fs::remove_dir_all("./index")
                        .expect("Couldn't remove index dir.");

                    match uwin::CourseIndex::open() {
                        Ok(index) => {
                            data.lock()
                                .insert::<uwin::CourseIndex>(index);
                        }
                        Err(e) => error!("Error while indexing: {}", e),
                    }

                    shard.set_game(None);
                });
            }
        }

        Ok(())
    }
}

impl EventHandler for Handler {
    fn message(&self, ctx: Context, msg: Message) {
        let (cmd, s) = {
            let len = msg.content
                .split_whitespace()
                .next()
                .map(|s| s.len())
                .unwrap_or(0);

            msg.content.split_at(len)
        };

        let s = s.trim();

        let cmd = match cmd {
            "~course" => self.fetch_course(ctx, s, msg.channel_id),
            "~reindex" => self.reindex(ctx, msg.member()),
            _ => Ok(()),
        };

        if let Err(e) = cmd {
            error!("Error attempting command: {}", e);

            let _ = msg.channel_id
                .send_message(|m| m.content("Internal error."));
        }
    }
}
