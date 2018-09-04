extern crate failure;
#[macro_use] extern crate failure_derive;
extern crate flexi_logger;
extern crate itertools;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
extern crate rayon;
extern crate regex;
extern crate reqwest;
extern crate select;
extern crate serenity;
extern crate tantivy;
extern crate typemap;

use std::{ fs, thread };

use failure::{ Error, SyncFailure };
use itertools::Itertools;
use regex::Regex;
use serenity::{
    CACHE,
    prelude::*,
    model::{
        channel::Message,
        gateway::{ Game, Ready },
        guild::Member,
        id::ChannelId,
        permissions::Permissions,
    },
};

static IMAGE_DATA: &[u8] = include_bytes!("../uw_logo.png");
const EMBED_COLOR: u32 = 0x00005696;
const DEFAULT_TERM: &str = "20185";

lazy_static! {
    static ref REGEX: Regex = Regex::new(r"([fsw])(\d\d)").unwrap();
}

mod uwin;

fn main() {
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

    fn fetch_course<'a, A>(&self, ctx: Context, mut args: A, chan: ChannelId) -> Result<(), Error>
        where A: Iterator<Item = &'a str>
    {
        let (term, query) = match args.next() {
            Some("-h") => {
                chan.send_message(|m| m.embed(|e| {
                        e.color(EMBED_COLOR)
                            .field("Usage", "~course [OPTION] <QUERY>", false)
                            .field("Options", "`-h` View the command help.\n`-s <[fsw]XX>` Select a semester where f (Fall) s (Summer) w (Winter) and XX is the year", false)
                            .field("Examples", "~course 60100\n~course graph theory\n~course -s f18 graph theory", false)
                    }))
                    .map_err(SyncFailure::new)?;

                return Ok(());
            }
            Some("-s") => {
                // Term codes are in the form [YEAR][CODE] where year is XXXX and
                // code is 1 (Winder) 2 (Summer) or 5 (Fall)
                // The bot will allow a user to enter [wWsSfF]XX
                let term = args.next()
                    .and_then(|s| REGEX.captures(s))
                    .and_then(|c| {
                        let term = match c.get(1)?.as_str() {
                            "w" | "W" => 1,
                            "s" | "S" => 2,
                            "f" | "F" => 5,
                            _ => return None,
                        };
                        let year = c.get(2)?.as_str();
                        Some(format!("20{}{}", year, term))
                    });

                if let Some(term) = term {
                    (term, args.join(" "))
                } else {
                    chan.send_message(|m| {
                            m.content("Semester selection is invalid.")
                        })
                        .map_err(SyncFailure::new)?;

                    return Ok(());
                }
            }
            s => {
                // Because we popped off the first term attempting to match a
                // command we have to make sure to join that back with the
                // rest of the arguments for the query.
                let query = s.into_iter()
                    .chain(args)
                    .join(" ");

                (DEFAULT_TERM.to_owned(), query)
            }
        };

        // The course index may not exist if we are reindexing.
        let data = ctx.data.lock();
        let index = match data.get::<uwin::CourseIndex>() {
            Some(index) => index,
            _ => return Ok(()),
        };

        // Make the bot seem like it's typing just in case this query
        // takes longer than expected.
        chan.broadcast_typing()
            .map_err(SyncFailure::new)?;

        let mut courses = match index.query(&term, &query) {
            Ok(courses) => courses,
            Err(e) => {
                return match e.downcast::<uwin::QueryError>() {
                    Ok(e) => {
                        // If the error is a query error we want to send a message in chat
                        // telling the user the query was invalid.
                        warn!("{}", e);

                        chan.send_message(|m| {
                                m.content(&format_args!("Query `\"{}\"` is invalid.", query))
                            })
                            .map_err(SyncFailure::new)?;

                        Ok(())
                    }
                    Err(e) => Err(e),
                };
            }
        };

        // Sort the courses in order by code.
        courses.sort_by(|c, other| c.code.cmp(&other.code));

        match courses.as_slice() {
            [] => {
                chan.send_message(|m| {
                        m.content(format!("No course found for query `\"{}\"`.", query))
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

                let files = vec![(IMAGE_DATA, "icon.png")];
                chan.send_files(files, |m| m.embed(|e| {
                        e.color(EMBED_COLOR)
                            .thumbnail("attachment://icon.png")
                            .title("Top 10 Results")
                            .description(courses)
                    }))
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
                let data = ctx.data.clone();
                thread::spawn(move || {
                    fs::remove_dir_all("./index")
                        .expect("Couldn't remove index dir.");

                    match uwin::CourseIndex::open() {
                        Ok(index) => {
                            data.lock()
                                .insert::<uwin::CourseIndex>(index);
                        }
                        Err(e) => error!("Error while indexing: {}", e),
                    }
                });
            }
        }

        Ok(())
    }
}

impl EventHandler for Handler {
    fn ready(&self, ctx: Context, _: Ready) {
        ctx.shard.set_game(Some(Game::playing("~course -h")));
    }

    fn message(&self, ctx: Context, msg: Message) {
        // Make sure we can send messages in this channel.
        let can_send = msg.channel()
            .and_then(|c| c.guild())
            .map(|c| {
                let current_user = CACHE.read().user.id;
                c.read()
                    .permissions_for(current_user)
                    .map(|p| p.contains(Permissions::SEND_MESSAGES | Permissions::ATTACH_FILES))
                    .unwrap_or(false)
            })
            .unwrap_or(true);

        if !can_send {
            return;
        }

        let mut args = msg.content
            .split_whitespace();

        let cmd = match args.next() {
            Some("~course") => self.fetch_course(ctx, args, msg.channel_id),
            Some("~reindex") => self.reindex(ctx, msg.member()),
            _ => return,
        };

        if let Err(e) = cmd {
            error!("Error attempting command: {}", e);

            let _ = msg.channel_id
                .send_message(|m| m.content("Internal error."));
        }
    }
}
