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

use std::{ fs, thread };

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
                let uwin::Course {
                    title,
                    description,
                    note,
                    meets,
                    instructors,
                    availability,
                    prereqs,
                    exams,
                    ..
                } = course.scrape()?;

                let description = description
                    .chars()
                    .take(200)
                    .chain("...\n\n".chars())
                    .join("");

                let mut fields = vec![];

                if let Some(note) = note {
                    fields.push(("Note", note, false));
                }

                fields.push(("Meets", meets, false));

                if !instructors.is_empty() {
                    let instructors = instructors
                        .into_iter()
                        .format_with("\n", |ins, f| {
                            if let Some(url) = ins.directory_url() {
                                f(&format_args!("[{}]({})", ins.name, url))
                            } else {
                                f(&format_args!("{}", ins.name))
                            }
                        })
                        .to_string();

                    fields.push(("Instructors", instructors, true));
                }

                fields.push(("Availability", availability, true));

                if !prereqs.is_empty() {
                    let prereqs = prereqs
                        .into_iter()
                        .join("\n");

                    fields.push(("Prerequisites", prereqs, false));
                }

                if !exams.is_empty() {
                    let exams = exams
                        .into_iter()
                        .format_with("\n", |ex, f| {
                            f(&format_args!("**{}**", ex.ty))?;

                            if let Some(date) = ex.date {
                                f(&format_args!(" on {}", date))?;
                            }

                            if let Some(time) = ex.time {
                                f(&format_args!(" at {}", time))?;
                            }

                            if let Some(building) = ex.building {
                                f(&format_args!(" in {}", building))?;
                            }

                            if let Some(room) = ex.room {
                                f(&format_args!(" room {}", room))?;
                            }

                            Ok(())
                        })
                        .to_string();

                    fields.push(("Exams", exams, false));
                }

                let files = vec![(IMAGE_DATA, "icon.png")];
                chan.send_files(files, |m| m.embed(|e| {
                        e.color(EMBED_COLOR)
                            .thumbnail("attachment://icon.png")
                            .title(title)
                            .description(description)
                            .fields(fields)
                    }))
                    .map_err(SyncFailure::new)?;
            }
            courses => {
                let courses = courses
                    .iter()
                    .format_with("\n", |course, f| {
                        f(&format_args!("`{}` {}", course.code, course.title))
                    });

                chan.send_message(|m| {
                        m.embed(|e| {
                            e.color(EMBED_COLOR)
                                .title("Top 10 Results")
                                .description(courses)
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
        let mut parts = msg.content
            .split_whitespace();

        let cmd = if let Some(cmd) = parts.next() {
            cmd
        } else {
            return;
        };

        let args = parts.join(" ");

        let cmd = match cmd {
            "~course" => self.fetch_course(ctx, &args, msg.channel_id),
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
