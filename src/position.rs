// This file is part of the shakmaty library.
// Copyright (C) 2017-2019 Niklas Fiekas <niklas.fiekas@backscattering.de>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use std::fmt;
use std::error::Error;
use std::num::NonZeroU32;

use bitflags::bitflags;

use crate::attacks;
use crate::board::Board;
use crate::bitboard::Bitboard;
use crate::square::{Rank, Square};
use crate::types::{Black, CastlingSide, CastlingMode, Color, Move, Piece, RemainingChecks, Role, White};
use crate::material::{Material, MaterialSide};
use crate::setup::{Castles, Setup, SwapTurn};
use crate::movelist::{ArrayVecExt, MoveList};

/// Outcome of a game.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Outcome {
    Decisive { winner: Color },
    Draw,
}

impl Outcome {
    pub fn winner(self) -> Option<Color> {
        match self {
            Outcome::Decisive { winner } => Some(winner),
            Outcome::Draw => None,
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", match *self {
            Outcome::Decisive { winner: White } => "1-0",
            Outcome::Decisive { winner: Black } => "0-1",
            Outcome::Draw => "1/2-1/2",
        })
    }
}

bitflags! {
    /// Reasons for a [`Setup`] not beeing a legal [`Position`].
    ///
    /// [`Setup`]: trait.Setup.html
    /// [`Position`]: trait.Position.html
    pub struct PositionErrorKind: u32 {
        const EMPTY_BOARD = 1 << 0;
        const MISSING_KING = 1 << 1;
        const TOO_MANY_KINGS = 1 << 2;
        const PAWNS_ON_BACKRANK = 1 << 3;
        const BAD_CASTLING_RIGHTS = 1 << 4;
        const INVALID_EP_SQUARE = 1 << 5;
        const OPPOSITE_CHECK = 1 << 6;
        const IMPOSSIBLE_CHECK = 1 << 7;
        const VARIANT = 1 << 8;
    }
}

/// Error when trying to create a [`Position`] from an illegal [`Setup`].
pub struct PositionError<P> {
    pub(crate) pos: Option<P>,
    pub(crate) errors: PositionErrorKind,
}

impl<P> PositionError<P> {
    fn ignore(mut self, ignore: PositionErrorKind) -> Result<P, Self> {
        self.errors -= ignore;
        match self {
            PositionError { pos: Some(pos), errors } if errors.is_empty() => Ok(pos),
            _ => Err(self),
        }
    }

    fn strict(self) -> Result<P, Self> {
        self.ignore(PositionErrorKind::empty())
    }

    pub fn ignore_bad_castling_rights(self) -> Result<P, Self> {
        self.ignore(PositionErrorKind::BAD_CASTLING_RIGHTS)
    }

    pub fn kind(&self) -> PositionErrorKind {
        self.errors
    }
}

impl<P> fmt::Debug for PositionError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PositionError")
            .field("errors", &self.errors)
            .finish()
    }
}

impl<P> fmt::Display for PositionError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        "illegal position".fmt(f)
    }
}

impl<P> Error for PositionError<P> {
    fn description(&self) -> &str {
        "illegal position"
    }
}

/// Validate and set up an arbitrary position. All provided chess variants
/// support this.
pub trait FromSetup: Sized {
    /// Set up a position.
    ///
    /// # Errors
    ///
    /// Returns [`PositionError`] if the setup does not meet basic validity
    /// requirements. Meeting the requirements does not imply that the position
    /// is actually reachable with a series of legal moves from the starting
    /// position.
    ///
    /// [`PositionError`]: enum.PositionError.html
    fn from_setup(setup: &dyn Setup) -> Result<Self, PositionError<Self>> {
        Self::from_setup_with_mode(setup, None)
    }

    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<Self, PositionError<Self>>;
}

/// A legal chess or chess variant position. See [`Chess`] for a concrete
/// implementation.
pub trait Position: Setup {
    /// Collects all legal moves in an existing buffer.
    fn legal_moves(&self, moves: &mut MoveList);

    /// Generates a subset of legal moves: All piece moves and drops of type
    /// `role` to the square `to`, excluding castling moves.
    fn san_candidates(&self, role: Role, to: Square, moves: &mut MoveList) {
        self.legal_moves(moves);
        filter_san_candidates(role, to, moves);
    }

    /// Generates legal castling moves.
    fn castling_moves(&self, side: CastlingSide, moves: &mut MoveList) {
        self.legal_moves(moves);
        moves.retain(|m| m.castling_side().map_or(false, |s| side == s));
    }

    /// Generates en passant moves.
    fn en_passant_moves(&self, moves: &mut MoveList) {
        self.legal_moves(moves);
        moves.retain(|m| m.is_en_passant());
    }

    /// Generates capture moves.
    fn capture_moves(&self, moves: &mut MoveList) {
        self.legal_moves(moves);
        moves.retain(|m| m.is_capture());
    }

    /// Generate promotion moves.
    fn promotion_moves(&self, moves: &mut MoveList) {
        self.legal_moves(moves);
        moves.retain(|m| m.is_promotion());
    }

    /// Tests if a move is irreversible.
    ///
    /// In standard chess pawn moves, captures, moves that destroy castling
    /// rights, and moves that cede en-passant are irreversible.
    ///
    /// The implementation has false-negatives, because it does not consider
    /// forced lines. For example a checking move that will force the king
    /// to lose castling rights is not considered irreversible, only the
    /// actual king move is.
    fn is_irreversible(&self, m: &Move) -> bool {
        (match *m {
            Move::Normal { role: Role::Pawn, .. } |
                Move::Normal { capture: Some(_), .. } |
                Move::Castle { .. } |
                Move::EnPassant { .. } |
                Move::Put { .. } => true,
            Move::Normal { role, from, to, .. } =>
                self.castling_rights().contains(from) ||
                self.castling_rights().contains(to) ||
                (role == Role::King && self.castles().has_side(self.turn()))
        }) || self.ep_square().is_some()
    }

    /// Attacks that a king on `square` would have to deal with.
    fn king_attackers(&self, square: Square, attacker: Color, occupied: Bitboard) -> Bitboard {
        self.board().attacks_to(square, attacker, occupied)
    }

    /// Castling paths and unmoved rooks.
    fn castles(&self) -> &Castles;

    /// Checks if the game is over due to a special variant end condition.
    ///
    /// Note that for example stalemate is not considered a variant-specific
    /// end condition (`is_variant_end()` will return `false`), but it can have
    /// a special [`variant_outcome()`](#tymethod.variant_outcome) in suicide
    /// chess.
    fn is_variant_end(&self) -> bool;

    /// Tests if a side has insufficient winning material.
    ///
    /// Returns `false` if there is any series of legal moves that allows
    /// `color` to win the game.
    ///
    /// The converse is not necessarily true: The position might be locked up
    /// such that `color` can never win the game (even if `!color` cooperates),
    /// or insufficient material might only become apparent after a forced
    /// sequence of moves.
    ///
    /// The current implementation can be summarized as follows: Looking
    /// only at the material configuration, taking into account if bishops
    /// are positioned on dark or light squares, but not concrete piece
    /// positions, is there a position with the same material configuration
    /// where `color` can win with a series of legal moves. If not, then
    /// `color` has insufficient winning material.
    fn has_insufficient_material(&self, color: Color) -> bool;

    /// Tests special variant winning, losing and drawing conditions.
    fn variant_outcome(&self) -> Option<Outcome>;

    /// Plays a move. It is the callers responsibility to ensure the move is
    /// legal.
    ///
    /// # Panics
    ///
    /// Illegal moves can corrupt the state of the position and may
    /// (or may not) panic or cause panics on future calls. Consider using
    /// [`Position::play()`](trait.Position.html#method.play) instead.
    fn play_unchecked(&mut self, m: &Move);

    // Implementation note: Trait methods above this comment should be made
    // available for VariantPosition. The provided methods below this comment
    // are never overwritten in implementations, but for simplicity of use
    // (especially around dyn) they are not moved to an extension trait.

    /// Swap turns. This is sometimes called "playing a null move".
    ///
    /// # Errors
    ///
    /// Returns [`PositionError`] if swapping turns is not possible (usually
    /// due to a check that has to be averted).
    ///
    /// [`PositionError`]: enum.PositionError.html
    fn swap_turn(self) -> Result<Self, PositionError<Self>>
    where
        Self: Sized + FromSetup
    {
        let mode = self.castles().mode();
        Self::from_setup_with_mode(&SwapTurn(self), Some(mode))
    }

    /// Generates legal moves.
    fn legals(&self) -> MoveList {
        let mut legals = MoveList::new();
        self.legal_moves(&mut legals);
        legals
    }

    /// Tests a move for legality.
    fn is_legal(&self, m: &Move) -> bool {
        let mut moves = MoveList::new();
        match *m {
            Move::Normal { role, to, .. } | Move::Put { role, to } =>
                self.san_candidates(role, to, &mut moves),
            Move::EnPassant { to, .. } =>
                self.san_candidates(Role::Pawn, to, &mut moves),
            Move::Castle { king, rook } if king.file() < rook.file() =>
                self.castling_moves(CastlingSide::KingSide, &mut moves),
            Move::Castle { .. } =>
                self.castling_moves(CastlingSide::QueenSide, &mut moves),
        }
        moves.contains(m)
    }

    /// Bitboard of pieces giving check.
    fn checkers(&self) -> Bitboard {
        self.our(Role::King).first().map_or(Bitboard(0), |king| {
            self.king_attackers(king, !self.turn(), self.board().occupied())
        })
    }

    /// Tests if the king is in check.
    fn is_check(&self) -> bool {
        self.checkers().any()
    }

    /// Tests for checkmate.
    fn is_checkmate(&self) -> bool {
        if self.checkers().is_empty() {
            return false;
        }

        let mut legals = MoveList::new();
        self.legal_moves(&mut legals);
        legals.is_empty()
    }

    /// Tests for stalemate.
    fn is_stalemate(&self) -> bool {
        if !self.checkers().is_empty() || self.is_variant_end() {
            false
        } else {
            let mut legals = MoveList::new();
            self.legal_moves(&mut legals);
            legals.is_empty()
        }
    }

    /// Tests if both sides
    /// [have insufficient winning material](#tymethod.has_insufficient_material).
    fn is_insufficient_material(&self) -> bool {
        self.has_insufficient_material(White) && self.has_insufficient_material(Black)
    }

    /// Tests if the game is over due to [checkmate](#method.is_checkmate),
    /// [stalemate](#method.is_stalemate),
    /// [insufficient material](#tymethod.is_insufficient_material) or
    /// [variant end](#tymethod.is_variant_end).
    fn is_game_over(&self) -> bool {
        let mut legals = MoveList::new();
        self.legal_moves(&mut legals);
        legals.is_empty() || self.is_insufficient_material()
    }

    /// The outcome of the game, or `None` if the game is not over.
    fn outcome(&self) -> Option<Outcome> {
        self.variant_outcome().or_else(|| {
            if self.is_checkmate() {
                Some(Outcome::Decisive { winner: !self.turn() })
            } else if self.is_insufficient_material() || self.is_stalemate() {
                Some(Outcome::Draw)
            } else {
                None
            }
        })
    }

    /// Plays a move.
    ///
    /// # Errors
    ///
    /// Returns the unchanged position if the move is not legal.
    fn play(mut self, m: &Move) -> Result<Self, Self>
    where
        Self: Sized,
    {
        if self.is_legal(m) {
            self.play_unchecked(m);
            Ok(self)
        } else {
            Err(self)
        }
    }
}

/// A standard Chess position.
#[derive(Clone, Debug)]
pub struct Chess {
    board: Board,
    turn: Color,
    castles: Castles,
    ep_square: Option<Square>,
    halfmoves: u32,
    fullmoves: NonZeroU32,
}

impl Chess {
    fn gives_check(&self, m: &Move) -> bool {
        let mut pos = self.clone();
        pos.play_unchecked(m);
        pos.is_check()
    }

    fn from_setup_with_mode_unchecked(setup: &dyn Setup, mode: Option<CastlingMode>) -> (Chess, PositionErrorKind) {
        let (castles, mut errors) = match Castles::from_setup_with_mode(setup, mode) {
            Ok(castles) => (castles, PositionErrorKind::empty()),
            Err(castles) => (castles, PositionErrorKind::BAD_CASTLING_RIGHTS),
        };

        let pos = Chess {
            board: setup.board().clone(),
            turn: setup.turn(),
            castles,
            ep_square: setup.ep_square(),
            halfmoves: setup.halfmoves(),
            fullmoves: setup.fullmoves(),
        };

        errors |= validate(&pos);

        (pos, errors)
    }
}

impl Default for Chess {
    fn default() -> Chess {
        Chess {
            board: Board::default(),
            turn: White,
            castles: Castles::default(),
            ep_square: None,
            halfmoves: 0,
            fullmoves: NonZeroU32::new(1).unwrap(),
        }
    }
}

impl Setup for Chess {
    fn board(&self) -> &Board { &self.board }
    fn pockets(&self) -> Option<&Material> { None }
    fn turn(&self) -> Color { self.turn }
    fn castling_rights(&self) -> Bitboard { self.castles.castling_rights() }
    fn ep_square(&self) -> Option<Square> { self.ep_square.filter(|_| has_relevant_ep(self)) }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { None }
    fn halfmoves(&self) -> u32 { self.halfmoves }
    fn fullmoves(&self) -> NonZeroU32 { self.fullmoves }
}

impl FromSetup for Chess {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<Chess, PositionError<Chess>> {
        let (pos, errors) = Chess::from_setup_with_mode_unchecked(setup, mode);
        PositionError {
            pos: Some(pos),
            errors,
        }.strict()
    }
}

impl Position for Chess {
    fn play_unchecked(&mut self, m: &Move) {
        do_move(&mut self.board, &mut self.turn, &mut self.castles,
                &mut self.ep_square, &mut self.halfmoves,
                &mut self.fullmoves, m);
    }

    fn castles(&self) -> &Castles {
        &self.castles
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        moves.clear();

        let king = self.board().king_of(self.turn()).expect("king in standard chess");

        let has_ep = gen_en_passant(self.board(), self.turn(), self.ep_square, moves);

        let checkers = self.checkers();
        if checkers.is_empty() {
            let target = !self.us();
            gen_non_king(self, target, moves);
            gen_safe_king(self, king, target, moves);
            gen_castling_moves(self, &self.castles, king, CastlingSide::KingSide, moves);
            gen_castling_moves(self, &self.castles, king, CastlingSide::QueenSide, moves);
        } else {
            evasions(self, king, checkers, moves);
        }

        let blockers = slider_blockers(self.board(), self.them(), king);
        if blockers.any() || has_ep {
            moves.swap_retain(|m| is_safe(self, king, m, blockers));
        }
    }

    fn castling_moves(&self, side: CastlingSide, moves: &mut MoveList) {
        moves.clear();
        let king = self.board().king_of(self.turn()).expect("king in standard chess");
        gen_castling_moves(self, &self.castles, king, side, moves);
    }

    fn en_passant_moves(&self, moves: &mut MoveList) {
        moves.clear();

        if gen_en_passant(self.board(), self.turn(), self.ep_square, moves) {
            let king = self.board().king_of(self.turn()).expect("king in standard chess");
            let blockers = slider_blockers(self.board(), self.them(), king);
            moves.swap_retain(|m| is_safe(self, king, m, blockers));
        }
    }

    fn promotion_moves(&self, moves: &mut MoveList) {
        moves.clear();

        let king = self.board().king_of(self.turn()).expect("king in standard chess");
        let checkers = self.checkers();

        if checkers.is_empty() {
            gen_pawn_moves(self, Bitboard::BACKRANKS, moves);
        } else {
            evasions(self, king, checkers, moves);
            moves.retain(|m| m.is_promotion());
        }

        let blockers = slider_blockers(self.board(), self.them(), king);
        if blockers.any() {
            moves.swap_retain(|m| is_safe(self, king, m, blockers));
        }
    }

    fn san_candidates(&self, role: Role, to: Square, moves: &mut MoveList) {
        moves.clear();

        let king = self.board().king_of(self.turn()).expect("king in standard chess");
        let checkers = self.checkers();

        if checkers.is_empty() {
            let piece_from = match role {
                Role::Pawn | Role::King => Bitboard(0),
                Role::Knight => attacks::knight_attacks(to),
                Role::Bishop => attacks::bishop_attacks(to, self.board().occupied()),
                Role::Rook => attacks::rook_attacks(to, self.board().occupied()),
                Role::Queen => attacks::queen_attacks(to, self.board().occupied()),
            };

            if !self.us().contains(to) {
                match role {
                    Role::Pawn => gen_pawn_moves(self, Bitboard::from_square(to), moves),
                    Role::King => gen_safe_king(self, king, Bitboard::from_square(to), moves),
                    _ => {}
                }

                for from in piece_from & self.our(role) {
                    moves.push(Move::Normal {
                        role,
                        from,
                        capture: self.board().role_at(to),
                        to,
                        promotion: None,
                    });
                }
            }
        } else {
            evasions(self, king, checkers, moves);
            filter_san_candidates(role, to, moves);
        }

        let has_ep =
            role == Role::Pawn &&
            Some(to) == self.ep_square &&
            gen_en_passant(self.board(), self.turn(), self.ep_square, moves);

        let blockers = slider_blockers(self.board(), self.them(), king);
        if blockers.any() || has_ep {
            moves.swap_retain(|m| is_safe(self, king, m, blockers));
        }
    }

    fn has_insufficient_material(&self, color: Color) -> bool {
        // Pawns, rooks and queens are never insufficient material.
        if (self.board.by_color(color) & (self.board.pawns() | self.board.rooks_and_queens())).any() {
            return false;
        }

        // Knights are only insufficient material if:
        // (1) We do not have any other pieces, including more than one knight.
        // (2) The opponent does not have pawns, knights, bishops or rooks.
        //     These would allow self mate.
        if (self.board.by_color(color) & self.board.knights()).any() {
            return self.board.by_color(color).count() <= 2 &&
                (self.board.by_color(!color) & !self.board.kings() & !self.board().queens()).is_empty();
        }

        // Bishops are only insufficient material if:
        // (1) We do not have any other pieces, including bishops on the
        //     opposite color.
        // (2) The opponent does not have bishops on the opposite color,
        //      pawns or knights. These would allow self mate.
        if (self.board.by_color(color) & self.board.bishops()).any() {
            let same_color =
                (self.board().bishops() & Bitboard::DARK_SQUARES).is_empty() ||
                (self.board().bishops() & Bitboard::LIGHT_SQUARES).is_empty();
            return same_color && self.board().knights().is_empty() && self.board().pawns().is_empty();
        }

        true
    }

    fn is_variant_end(&self) -> bool { false }
    fn variant_outcome(&self) -> Option<Outcome> { None }
}

/// An Atomic Chess position.
#[derive(Clone, Debug)]
pub struct Atomic {
    board: Board,
    turn: Color,
    castles: Castles,
    ep_square: Option<Square>,
    halfmoves: u32,
    fullmoves: NonZeroU32,
}

impl Default for Atomic {
    fn default() -> Atomic {
        Atomic {
            board: Board::default(),
            turn: White,
            castles: Castles::default(),
            ep_square: None,
            halfmoves: 0,
            fullmoves: NonZeroU32::new(1).unwrap(),
        }
    }
}

impl Setup for Atomic {
    fn board(&self) -> &Board { &self.board }
    fn pockets(&self) -> Option<&Material> { None }
    fn turn(&self) -> Color { self.turn }
    fn castling_rights(&self) -> Bitboard { self.castles.castling_rights() }
    fn ep_square(&self) -> Option<Square> { self.ep_square.filter(|_| has_relevant_ep(self)) }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { None }
    fn halfmoves(&self) -> u32 { self.halfmoves }
    fn fullmoves(&self) -> NonZeroU32 { self.fullmoves }
}

impl FromSetup for Atomic {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<Atomic, PositionError<Atomic>> {
        let (castles, errors) = match Castles::from_setup_with_mode(setup, mode) {
            Ok(castles) => (castles, PositionErrorKind::empty()),
            Err(castles) => (castles, PositionErrorKind::BAD_CASTLING_RIGHTS),
        };

        let pos = Atomic {
            board: setup.board().clone(),
            turn: setup.turn(),
            castles,
            ep_square: setup.ep_square(),
            halfmoves: setup.halfmoves(),
            fullmoves: setup.fullmoves(),
        };

        let mut errors = validate(&pos) | errors;

        if (pos.them() & pos.board().kings()).any() {
            // Our king just exploded. Game over, but valid position.
            errors.remove(PositionErrorKind::MISSING_KING);
        }

        PositionError {
            errors: (errors - PositionErrorKind::IMPOSSIBLE_CHECK),
            pos: Some(pos),
        }.strict()
    }
}

impl Position for Atomic {
    fn castles(&self) -> &Castles {
        &self.castles
    }

    fn play_unchecked(&mut self, m: &Move) {
        do_move(&mut self.board, &mut self.turn, &mut self.castles,
                &mut self.ep_square, &mut self.halfmoves,
                &mut self.fullmoves, m);

        match *m {
            Move::Normal { capture: Some(_), to, .. } | Move::EnPassant { to, .. } => {
                self.board.remove_piece_at(to);

                let explosion_radius = attacks::king_attacks(to) &
                                       self.board().occupied() &
                                       !self.board.pawns();

                if (explosion_radius & self.board().kings() & self.us()).any() {
                    self.castles.discard_side(self.turn());
                }

                for explosion in explosion_radius {
                    self.board.remove_piece_at(explosion);
                    self.castles.discard_rook(explosion);
                }
            },
            _ => ()
        }
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        moves.clear();

        gen_en_passant(self.board(), self.turn(), self.ep_square, moves);
        gen_non_king(self, !self.us(), moves);
        KingTag::gen_moves(self, !self.board().occupied(), moves);
        if let Some(king) = self.board().king_of(self.turn()) {
            gen_castling_moves(self, &self.castles, king, CastlingSide::KingSide, moves);
            gen_castling_moves(self, &self.castles, king, CastlingSide::QueenSide, moves);
        }

        // Atomic move generation could be implemented more efficiently.
        // For simplicity we filter all pseudo legal moves.
        moves.swap_retain(|m| {
            let mut after = self.clone();
            after.play_unchecked(m);
            if let Some(our_king) = after.board().king_of(self.turn()) {
                (after.board.kings() & after.board().by_color(!self.turn())).is_empty() ||
                after.king_attackers(our_king, !self.turn(), after.board.occupied()).is_empty()
            } else {
                false
            }
        });
    }

    fn king_attackers(&self, square: Square, attacker: Color, occupied: Bitboard) -> Bitboard {
        if (attacks::king_attacks(square) & self.board().kings() & self.board().by_color(attacker)).any() {
            Bitboard(0)
        } else {
            self.board().attacks_to(square, attacker, occupied)
        }
    }

    fn is_variant_end(&self) -> bool {
        self.variant_outcome().is_some()
    }

    fn has_insufficient_material(&self, color: Color) -> bool {
        // Remaining material does not matter if the opponents king is already
        // exploded.
        if (self.board.by_color(!color) & self.board.kings()).is_empty() {
            return false;
        }

        // Bare king can not mate.
        if (self.board.by_color(color) & !self.board.kings()).is_empty() {
            return true;
        }

        // As long as the opponent king is not alone there is always a chance
        // their own piece explodes next to it.
        if (self.board.by_color(!color) & !self.board.kings()).any() {
            // Unless there are only bishops that cannot explode each other.
            if self.board().occupied() == self.board().kings() | self.board().bishops() {
                if (self.board().bishops() & self.board().white() & Bitboard::DARK_SQUARES).is_empty() {
                    return (self.board().bishops() & self.board().black() & Bitboard::LIGHT_SQUARES).is_empty();
                }
                if (self.board().bishops() & self.board().white() & Bitboard::LIGHT_SQUARES).is_empty() {
                    return (self.board().bishops() & self.board().black() & Bitboard::DARK_SQUARES).is_empty();
                }
            }

            return false;
        }

        // Queen or pawn (future queen) can give mate against bare king.
        if self.board().queens().any() || self.board.pawns().any() {
            return false;
        }

        // Single knight, bishop or rook can not mate against bare king.
        if (self.board().knights() | self.board().bishops() | self.board().rooks()).count() == 1 {
            return true;
        }

        // Two knights can not mate against bare king.
        if self.board().occupied() == self.board().kings() | self.board().knights() {
            return self.board().knights().count() <= 2;
        }

        false
    }

    fn variant_outcome(&self) -> Option<Outcome> {
        for &color in &[White, Black] {
            if (self.board().by_color(color) & self.board().kings()).is_empty() {
                return Some(Outcome::Decisive { winner: !color });
            }
        }
        None
    }
}

/// An Antichess position. Antichess is also known as Giveaway, but players
/// start without castling rights.
#[derive(Clone, Debug)]
pub struct Antichess {
    board: Board,
    turn: Color,
    castles: Castles,
    ep_square: Option<Square>,
    halfmoves: u32,
    fullmoves: NonZeroU32,
}

impl Default for Antichess {
    fn default() -> Antichess {
        Antichess {
            board: Board::default(),
            turn: White,
            castles: Castles::empty(CastlingMode::Standard),
            ep_square: None,
            halfmoves: 0,
            fullmoves: NonZeroU32::new(1).unwrap(),
        }
    }
}

impl Setup for Antichess {
    fn board(&self) -> &Board { &self.board }
    fn pockets(&self) -> Option<&Material> { None }
    fn turn(&self) -> Color { self.turn }
    fn castling_rights(&self) -> Bitboard { Bitboard(0) }
    fn ep_square(&self) -> Option<Square> { self.ep_square.filter(|_| has_relevant_ep(self)) }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { None }
    fn halfmoves(&self) -> u32 { self.halfmoves }
    fn fullmoves(&self) -> NonZeroU32 { self.fullmoves }
}

impl FromSetup for Antichess {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<Antichess, PositionError<Antichess>> {
        let pos = Antichess {
            board: setup.board().clone(),
            turn: setup.turn(),
            castles: Castles::empty(mode.unwrap_or_default()),
            ep_square: setup.ep_square(),
            halfmoves: setup.halfmoves(),
            fullmoves: setup.fullmoves(),
        };

        let errors = if setup.castling_rights().any() {
            PositionErrorKind::BAD_CASTLING_RIGHTS
        } else {
            PositionErrorKind::empty()
        };

        let errors = (validate(&pos) | errors)
            - PositionErrorKind::MISSING_KING
            - PositionErrorKind::TOO_MANY_KINGS
            - PositionErrorKind::OPPOSITE_CHECK
            - PositionErrorKind::IMPOSSIBLE_CHECK;

        PositionError {
            errors,
            pos: Some(pos),
        }.strict()
    }
}

impl Position for Antichess {
    fn play_unchecked(&mut self, m: &Move) {
        do_move(&mut self.board, &mut self.turn, &mut self.castles,
                &mut self.ep_square, &mut self.halfmoves,
                &mut self.fullmoves, m);
    }

    fn castles(&self) -> &Castles {
        &self.castles
    }

    fn en_passant_moves(&self, moves: &mut MoveList) {
        moves.clear();
        gen_en_passant(self.board(), self.turn, self.ep_square, moves);
    }

    fn capture_moves(&self, moves: &mut MoveList) {
        self.en_passant_moves(moves); // clears move list
        let them = self.them();
        gen_non_king(self, them, moves);
        add_king_promotions(moves);
        KingTag::gen_moves(self, them, moves);
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        self.capture_moves(moves); // clears move list

        if moves.is_empty() {
            // No compulsory captures. Generate everything else.
            gen_non_king(self, !self.board().occupied(), moves);
            add_king_promotions(moves);
            KingTag::gen_moves(self, !self.board().occupied(), moves);
        }
    }

    fn king_attackers(&self, _square: Square, _attacker: Color, _occupied: Bitboard) -> Bitboard {
        Bitboard(0)
    }

    fn is_variant_end(&self) -> bool {
        self.board().white().is_empty() || self.board().black().is_empty()
    }

    fn has_insufficient_material(&self, color: Color) -> bool {
        // In a position with only bishops, check if all our bishops can be
        // captured.
        if self.board.occupied() == self.board.bishops() {
            let we_some_on_light = (self.board.by_color(color) & Bitboard::LIGHT_SQUARES).any();
            let we_some_on_dark = (self.board.by_color(color) & Bitboard::DARK_SQUARES).any();
            let they_all_on_dark = (self.board.by_color(!color) & Bitboard::LIGHT_SQUARES).is_empty();
            let they_all_on_light = (self.board.by_color(!color) & Bitboard::DARK_SQUARES).is_empty();
            (we_some_on_light && they_all_on_dark) || (we_some_on_dark && they_all_on_light)
        } else {
            false
        }
    }

    fn variant_outcome(&self) -> Option<Outcome> {
        if self.us().is_empty() || self.is_stalemate() {
            Some(Outcome::Decisive { winner: self.turn() })
        } else {
            None
        }
    }
}

/// A King of the Hill position.
#[derive(Clone, Debug, Default)]
pub struct KingOfTheHill {
    chess: Chess,
}

impl Setup for KingOfTheHill {
    fn board(&self) -> &Board { self.chess.board() }
    fn pockets(&self) -> Option<&Material> { None }
    fn turn(&self) -> Color { self.chess.turn() }
    fn castling_rights(&self) -> Bitboard { self.chess.castling_rights() }
    fn ep_square(&self) -> Option<Square> { self.chess.ep_square() }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { None }
    fn halfmoves(&self) -> u32 { self.chess.halfmoves() }
    fn fullmoves(&self) -> NonZeroU32 { self.chess.fullmoves() }
}

impl FromSetup for KingOfTheHill {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<KingOfTheHill, PositionError<KingOfTheHill>> {
        let (chess, errors) = Chess::from_setup_with_mode_unchecked(setup, mode);
        PositionError {
            errors,
            pos: Some(KingOfTheHill { chess }),
        }.strict()
    }
}

impl Position for KingOfTheHill {
    fn play_unchecked(&mut self, m: &Move) {
        self.chess.play_unchecked(m);
    }

    fn castles(&self) -> &Castles {
        self.chess.castles()
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.legal_moves(moves);
        }
    }

    fn castling_moves(&self, side: CastlingSide, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.castling_moves(side, moves);
        }
    }

    fn en_passant_moves(&self, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.en_passant_moves(moves);
        }
    }

    fn san_candidates(&self, role: Role, to: Square, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.san_candidates(role, to, moves);
        }
    }

    fn has_insufficient_material(&self, _color: Color) -> bool {
        // Even a lone king can walk onto the hill.
        false
    }

    fn is_variant_end(&self) -> bool {
        (self.chess.board().kings() & Bitboard::CENTER).any()
    }

    fn variant_outcome(&self) -> Option<Outcome> {
        for &color in &[White, Black] {
            if (self.board().by_color(color) & self.board().kings() & Bitboard::CENTER).any() {
                return Some(Outcome::Decisive { winner: color });
            }
        }
        None
    }
}

/// A Three-Check position.
#[derive(Clone, Debug, Default)]
pub struct ThreeCheck {
    chess: Chess,
    remaining_checks: RemainingChecks,
}

impl Setup for ThreeCheck {
    fn board(&self) -> &Board { self.chess.board() }
    fn pockets(&self) -> Option<&Material> { None }
    fn turn(&self) -> Color { self.chess.turn() }
    fn castling_rights(&self) -> Bitboard { self.chess.castling_rights() }
    fn ep_square(&self) -> Option<Square> { self.chess.ep_square() }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { Some(&self.remaining_checks) }
    fn halfmoves(&self) -> u32 { self.chess.halfmoves() }
    fn fullmoves(&self) -> NonZeroU32 { self.chess.fullmoves }
}

impl FromSetup for ThreeCheck {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<ThreeCheck, PositionError<ThreeCheck>> {
        let (chess, mut errors) = Chess::from_setup_with_mode_unchecked(setup, mode);

        let remaining_checks = setup.remaining_checks().cloned().unwrap_or_default();
        if remaining_checks.white == 0 && remaining_checks.black == 0 {
            errors |= PositionErrorKind::VARIANT
        }

        PositionError {
            errors,
            pos: Some(ThreeCheck { chess, remaining_checks }),
        }.strict()
    }
}

impl Position for ThreeCheck {
    fn play_unchecked(&mut self, m: &Move) {
        let turn = self.chess.turn();
        self.chess.play_unchecked(m);
        if self.is_check() {
            self.remaining_checks.decrement(turn);
        }
    }

    fn castles(&self) -> &Castles {
        self.chess.castles()
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.legal_moves(moves);
        }
    }

    fn castling_moves(&self, side: CastlingSide, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.castling_moves(side, moves);
        }
    }

    fn en_passant_moves(&self, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.en_passant_moves(moves);
        }
    }

    fn san_candidates(&self, role: Role, to: Square, moves: &mut MoveList) {
        if self.is_variant_end() {
            moves.clear();
        } else {
            self.chess.san_candidates(role, to, moves);
        }
    }

    fn has_insufficient_material(&self, color: Color) -> bool {
        // Any remaining piece can give check.
        (self.board().by_color(color) & !self.board().kings()).is_empty()
    }

    fn is_irreversible(&self, m: &Move) -> bool {
        self.chess.is_irreversible(m) || self.chess.gives_check(m)
    }

    fn is_variant_end(&self) -> bool {
        self.remaining_checks.white == 0 || self.remaining_checks.black == 0
    }

    fn variant_outcome(&self) -> Option<Outcome> {
        if self.remaining_checks.white == 0 && self.remaining_checks.black == 0 {
            Some(Outcome::Draw)
        } else if self.remaining_checks.white == 0 {
            Some(Outcome::Decisive { winner: White })
        } else if self.remaining_checks.black == 0 {
            Some(Outcome::Decisive { winner: Black })
        } else {
            None
        }
    }
}

/// A Crazyhouse position.
#[derive(Clone, Debug, Default)]
pub struct Crazyhouse {
    chess: Chess,
    pockets: Material,
}

impl Crazyhouse {
    fn our_pocket(&self) -> &MaterialSide {
        self.pockets.by_color(self.turn())
    }

    fn our_pocket_mut(&mut self) -> &mut MaterialSide {
        let turn = self.turn();
        self.pockets.by_color_mut(turn)
    }

    fn legal_put_squares(&self) -> Bitboard {
        let checkers = self.checkers();

        if checkers.is_empty() {
            !self.board().occupied()
        } else if let Some(checker) = checkers.single_square() {
            let king = self.board().king_of(self.turn()).expect("king in crazyhouse");
            attacks::between(checker, king)
        } else {
            Bitboard(0)
        }
    }
}

impl Setup for Crazyhouse {
    fn board(&self) -> &Board { self.chess.board() }
    fn pockets(&self) -> Option<&Material> { Some(&self.pockets) }
    fn turn(&self) -> Color { self.chess.turn() }
    fn castling_rights(&self) -> Bitboard { self.chess.castling_rights() }
    fn ep_square(&self) -> Option<Square> { self.chess.ep_square() }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { None }
    fn halfmoves(&self) -> u32 { self.chess.halfmoves() }
    fn fullmoves(&self) -> NonZeroU32 { self.chess.fullmoves() }
}

impl FromSetup for Crazyhouse {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<Crazyhouse, PositionError<Crazyhouse>> {
        let (chess, mut errors) = Chess::from_setup_with_mode_unchecked(setup, mode);

        let pockets = setup.pockets().cloned().unwrap_or_default();
        if pockets.count().saturating_add(chess.board().occupied().count()) > 64 {
            errors |= PositionErrorKind::VARIANT;
        } else if pockets.white.kings > 0 || pockets.black.kings > 0 {
            errors |= PositionErrorKind::TOO_MANY_KINGS;
        }

        PositionError {
            errors,
            pos: Some(Crazyhouse { chess, pockets }),
        }.strict()
    }
}

impl Position for Crazyhouse {
    fn play_unchecked(&mut self, m: &Move) {
        match *m {
            Move::Normal { capture: Some(capture), to, .. } => {
                let capture = if self.board().promoted().contains(to) {
                    Role::Pawn
                } else {
                    capture
                };

                *self.our_pocket_mut().by_role_mut(capture) += 1;
            }
            Move::EnPassant { .. } => {
                self.our_pocket_mut().pawns += 1;
            }
            Move::Put { role, .. } => {
                *self.our_pocket_mut().by_role_mut(role) -= 1;
            }
            _ => {}
        }

        self.chess.play_unchecked(m);
    }

    fn castles(&self) -> &Castles {
        self.chess.castles()
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        self.chess.legal_moves(moves);

        let pocket = self.our_pocket();
        let targets = self.legal_put_squares();

        for to in targets {
            for &role in &[Role::Knight, Role::Bishop, Role::Rook, Role::Queen] {
                if pocket.by_role(role) > 0 {
                    moves.push(Move::Put { role, to });
                }
            }
        }

        if pocket.pawns > 0 {
            for to in targets & !Bitboard::BACKRANKS {
                moves.push(Move::Put { role: Role::Pawn, to });
            }
        }
    }

    fn castling_moves(&self, side: CastlingSide, moves: &mut MoveList) {
        self.chess.castling_moves(side, moves);
    }

    fn en_passant_moves(&self, moves: &mut MoveList) {
        self.chess.en_passant_moves(moves);
    }

    fn san_candidates(&self, role: Role, to: Square, moves: &mut MoveList) {
        self.chess.san_candidates(role, to, moves);

        if self.our_pocket().by_role(role) > 0 && self.legal_put_squares().contains(to) &&
           (role != Role::Pawn || !Bitboard::BACKRANKS.contains(to))
        {
            moves.push(Move::Put { role, to });
        }
    }

    fn is_irreversible(&self, m: &Move) -> bool {
        match *m {
            Move::Castle { .. } => true,
            Move::Normal { role, from, to, .. } =>
                self.castling_rights().contains(from) ||
                self.castling_rights().contains(to) ||
                (role == Role::King && self.chess.castles.has_side(self.turn())),
            _ => false,
        }
    }

    fn has_insufficient_material(&self, _color: Color) -> bool {
        // In practise no material can leave the game, but this is simple
        // to implement anyway. Bishops can be captured and put onto a
        // different color complex.
        self.board().occupied().count() + self.pockets.count() <= 3 &&
        self.board().promoted().is_empty() &&
        self.board().pawns().is_empty() &&
        self.board().rooks_and_queens().is_empty() &&
        self.pockets.white.pawns == 0 &&
        self.pockets.black.pawns == 0 &&
        self.pockets.white.rooks == 0 &&
        self.pockets.black.rooks == 0 &&
        self.pockets.white.queens == 0 &&
        self.pockets.black.queens == 0
    }

    fn is_variant_end(&self) -> bool { false }
    fn variant_outcome(&self) -> Option<Outcome> { None }
}

/// A Racing Kings position.
#[derive(Clone, Debug)]
pub struct RacingKings {
    board: Board,
    turn: Color,
    castles: Castles,
    halfmoves: u32,
    fullmoves: NonZeroU32,
}

impl Default for RacingKings {
    fn default() -> RacingKings {
        RacingKings {
            board: Board::racing_kings(),
            turn: White,
            castles: Castles::empty(CastlingMode::Standard),
            halfmoves: 0,
            fullmoves: NonZeroU32::new(1).unwrap(),
        }
    }
}

impl Setup for RacingKings {
    fn board(&self) -> &Board { &self.board }
    fn pockets(&self) -> Option<&Material> { None }
    fn turn(&self) -> Color { self.turn }
    fn castling_rights(&self) -> Bitboard { Bitboard(0) }
    fn ep_square(&self) -> Option<Square> { None }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { None }
    fn halfmoves(&self) -> u32 { self.halfmoves }
    fn fullmoves(&self) -> NonZeroU32 { self.fullmoves }
}

impl FromSetup for RacingKings {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<RacingKings, PositionError<RacingKings>> {
        let mut errors = PositionErrorKind::empty();

        if setup.castling_rights().any() {
            errors |= PositionErrorKind::BAD_CASTLING_RIGHTS;
        }

        let board = setup.board().clone();
        if board.pawns().any() {
            errors |= PositionErrorKind::VARIANT;
        }
        if setup.ep_square().is_some() {
            errors |= PositionErrorKind::INVALID_EP_SQUARE;
        }

        let pos = RacingKings {
            board,
            turn: setup.turn(),
            castles: Castles::empty(mode.unwrap_or_default()),
            halfmoves: setup.halfmoves(),
            fullmoves: setup.fullmoves(),
        };

        if pos.is_check() {
            errors |= PositionErrorKind::IMPOSSIBLE_CHECK;
        }

        if pos.turn().is_black() &&
           (pos.board().white() & pos.board().kings() & Rank::Eighth).any() &&
           (pos.board().black() & pos.board().kings() & Rank::Eighth).any()
        {
            errors |= PositionErrorKind::VARIANT;
        }

        PositionError {
            errors: validate(&pos) | errors,
            pos: Some(pos),
        }.strict()
    }
}

impl Position for RacingKings {
    fn play_unchecked(&mut self, m: &Move) {
        do_move(&mut self.board, &mut self.turn, &mut self.castles,
                &mut None, &mut self.halfmoves,
                &mut self.fullmoves, m);
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        moves.clear();

        if self.is_variant_end() {
            return;
        }

        // Generate all legal moves (no castling, no ep).
        let target = !self.us();
        gen_non_king(self, target, moves);
        let king = self.board().king_of(self.turn()).expect("king in racingkings");
        gen_safe_king(self, king, target, moves);

        let blockers = slider_blockers(self.board(), self.them(), king);
        if blockers.any() {
            moves.swap_retain(|m| is_safe(self, king, m, blockers));
        }

        // Do not allow giving check. This could be implemented more
        // efficiently.
        moves.swap_retain(|m| {
            let mut after = self.clone();
            after.play_unchecked(m);
            !after.is_check()
        });
    }

    fn castles(&self) -> &Castles {
        &self.castles
    }

    fn has_insufficient_material(&self, _color: Color) -> bool {
        // Even a lone king can win the race.
        false
    }

    fn is_variant_end(&self) -> bool {
        let in_goal = self.board().kings() & Rank::Eighth;
        if in_goal.is_empty() {
            return false;
        }

        if self.turn().is_white() || (in_goal & self.board().black()).any() {
            return true;
        }

        // White has reached the backrank. Check if black can catch up.
        let black_king = self.board().king_of(Black).expect("king in racingkings");
        for target in attacks::king_attacks(black_king) & Rank::Eighth & !self.board().black() {
            if self.king_attackers(target, White, self.board().occupied()).is_empty() {
                return false;
            }
        }

        true
    }

    fn variant_outcome(&self) -> Option<Outcome> {
        if self.is_variant_end() {
            let in_goal = self.board().kings() & Rank::Eighth;
            if (in_goal & self.board().white()).any() && (in_goal & self.board().black()).any() {
                Some(Outcome::Draw)
            } else if (in_goal & self.board().white()).any() {
                Some(Outcome::Decisive { winner: White })
            } else {
                Some(Outcome::Decisive { winner: Black })
            }
        } else {
            None
        }
    }
}

/// A Horde position.
#[derive(Clone, Debug)]
pub struct Horde {
    board: Board,
    turn: Color,
    castles: Castles,
    ep_square: Option<Square>,
    halfmoves: u32,
    fullmoves: NonZeroU32,
}

impl Default for Horde {
    fn default() -> Horde {
        let mut castles = Castles::default();
        castles.discard_side(White);

        Horde {
            board: Board::horde(),
            turn: White,
            castles,
            ep_square: None,
            halfmoves: 0,
            fullmoves: NonZeroU32::new(1).unwrap(),
        }
    }
}

impl Setup for Horde {
    fn board(&self) -> &Board { &self.board }
    fn pockets(&self) -> Option<&Material> { None }
    fn turn(&self) -> Color { self.turn }
    fn castling_rights(&self) -> Bitboard { self.castles.castling_rights() }
    fn ep_square(&self) -> Option<Square> { self.ep_square.filter(|_| has_relevant_ep(self)) }
    fn remaining_checks(&self) -> Option<&RemainingChecks> { None }
    fn halfmoves(&self) -> u32 { self.halfmoves }
    fn fullmoves(&self) -> NonZeroU32 { self.fullmoves }
}

impl FromSetup for Horde {
    fn from_setup_with_mode(setup: &dyn Setup, mode: Option<CastlingMode>) -> Result<Horde, PositionError<Horde>> {
        let (castles, errors) = match Castles::from_setup_with_mode(setup, mode) {
            Ok(castles) => (castles, PositionErrorKind::empty()),
            Err(castles) => (castles, PositionErrorKind::BAD_CASTLING_RIGHTS),
        };

        let pos = Horde {
            board: setup.board().clone(),
            turn: setup.turn(),
            castles,
            ep_square: setup.ep_square(),
            halfmoves: setup.halfmoves(),
            fullmoves: setup.fullmoves(),
        };

        let mut errors = (errors | validate(&pos))
            - PositionErrorKind::PAWNS_ON_BACKRANK
            - PositionErrorKind::MISSING_KING;

        if (pos.board().pawns() & pos.board().white() & Rank::Eighth).any() ||
           (pos.board().pawns() & pos.board().black() & Rank::First).any()
        {
            errors |= PositionErrorKind::PAWNS_ON_BACKRANK;
        }

        if (pos.board().kings() & !pos.board().promoted()).is_empty() {
            errors |= PositionErrorKind::MISSING_KING;
        }

        if (pos.board().kings() & pos.board().white()).any() &&
           (pos.board().kings() & pos.board().black()).any()
        {
            errors |= PositionErrorKind::VARIANT;
        }

        PositionError {
            errors,
            pos: Some(pos)
        }.strict()
    }
}

impl Position for Horde {
    fn play_unchecked(&mut self, m: &Move) {
        do_move(&mut self.board, &mut self.turn, &mut self.castles,
                &mut self.ep_square, &mut self.halfmoves,
                &mut self.fullmoves, m);
    }

    fn legal_moves(&self, moves: &mut MoveList) {
        moves.clear();

        let king = self.board().king_of(self.turn());
        let has_ep = gen_en_passant(self.board(), self.turn(), self.ep_square, moves);

        let checkers = self.checkers();
        if checkers.is_empty() {
            let target = !self.us();
            gen_non_king(self, target, moves);
            if let Some(king) = king {
                gen_safe_king(self, king, target, moves);
                gen_castling_moves(self, &self.castles, king, CastlingSide::KingSide, moves);
                gen_castling_moves(self, &self.castles, king, CastlingSide::QueenSide, moves);
            }
        } else {
            evasions(self, king.expect("king in check"), checkers, moves);
        }

        if let Some(king) = king {
            let blockers = slider_blockers(self.board(), self.them(), king);
            if blockers.any() || has_ep {
                moves.swap_retain(|m| is_safe(self, king, m, blockers));
            }
        }
    }

    fn castles(&self) -> &Castles {
        &self.castles
    }

    fn is_variant_end(&self) -> bool {
        self.board().white().is_empty() || self.board().black().is_empty()
    }

    fn has_insufficient_material(&self, color: Color) -> bool {
        // The side with the king can always win by capturing the horde.
        if (self.board.by_color(color) & self.board.kings()).any() {
            return false;
        }

        // TODO: Detect when the horde can not mate. Note that it does not have
        // a king.
        false
    }

    fn variant_outcome(&self) -> Option<Outcome> {
        if self.board().occupied().is_empty() {
            Some(Outcome::Draw)
        } else if self.board().white().is_empty() {
            Some(Outcome::Decisive { winner: Black })
        } else if self.board().black().is_empty() {
            Some(Outcome::Decisive { winner: White })
        } else {
            None
        }
    }
}

fn do_move(board: &mut Board,
           turn: &mut Color,
           castles: &mut Castles,
           ep_square: &mut Option<Square>,
           halfmoves: &mut u32,
           fullmoves: &mut NonZeroU32,
           m: &Move) {
    let color = *turn;
    ep_square.take();

    *halfmoves = if m.is_zeroing() {
        0
    } else {
        halfmoves.saturating_add(1)
    };

    match *m {
        Move::Normal { role, from, capture, to, promotion } => {
            if role == Role::Pawn && to - from == 16 && from.rank() == Rank::Second {
                *ep_square = from.offset(8);
            } else if role == Role::Pawn && from - to == 16 && from.rank() == Rank::Seventh {
                *ep_square = from.offset(-8);
            }

            if role == Role::King {
                castles.discard_side(color);
            } else if role == Role::Rook {
                castles.discard_rook(from);
            }

            if capture == Some(Role::Rook) {
                castles.discard_rook(to);
            }

            let promoted = board.promoted().contains(from) || promotion.is_some();

            board.discard_piece_at(from);
            board.set_piece_at(to, promotion.map_or(role.of(color), |p| p.of(color)), promoted);
        },
        Move::Castle { king, rook } => {
            let side = CastlingSide::from_queen_side(rook < king);
            board.discard_piece_at(king);
            board.discard_piece_at(rook);
            board.set_piece_at(Square::from_coords(side.rook_to_file(), rook.rank()), color.rook(), false);
            board.set_piece_at(Square::from_coords(side.king_to_file(), king.rank()), color.king(), false);
            castles.discard_side(color);
        }
        Move::EnPassant { from, to } => {
            board.discard_piece_at(Square::from_coords(to.file(), from.rank())); // captured pawn
            board.discard_piece_at(from);
            board.set_piece_at(to, color.pawn(), false);
        }
        Move::Put { role, to } => {
            board.set_piece_at(to, Piece { color, role }, false);
        }
    }

    if color.is_black() {
        *fullmoves = NonZeroU32::new(fullmoves.get().saturating_add(1)).unwrap();
    }

    *turn = !color;
}

fn validate<P: Position>(pos: &P) -> PositionErrorKind {
    let mut errors = PositionErrorKind::empty();

    if pos.board().occupied().is_empty() {
        errors |= PositionErrorKind::EMPTY_BOARD;
    }

    if (pos.board().pawns() & Bitboard::BACKRANKS).any() {
        errors |= PositionErrorKind::PAWNS_ON_BACKRANK;
    }

    // validate en passant square
    if let Some(ep_square) = pos.ep_square() {
        if !Bitboard::relative_rank(pos.turn(), Rank::Sixth).contains(ep_square) {
            errors |= PositionErrorKind::INVALID_EP_SQUARE;
        } else {
            let fifth_rank_sq = ep_square
                .offset(pos.turn().fold(-8, 8))
                .expect("ep square is on sixth rank");

            let seventh_rank_sq = ep_square
                .offset(pos.turn().fold(8, -8))
                .expect("ep square is on sixth rank");

            // The last move must have been a double pawn push. Check for the
            // presence of that pawn.
            if !pos.their(Role::Pawn).contains(fifth_rank_sq) {
                errors |= PositionErrorKind::INVALID_EP_SQUARE;
            }

            if pos.board().occupied().contains(ep_square) || pos.board().occupied().contains(seventh_rank_sq) {
                errors |= PositionErrorKind::INVALID_EP_SQUARE;
            }
        }
    }

    for &color in &[White, Black] {
        if pos.board().king_of(color).is_none() {
            errors |= PositionErrorKind::MISSING_KING;
        }
    }

    if (pos.board().kings() & pos.board().white()).more_than_one() ||
       (pos.board().kings() & pos.board().black()).more_than_one()
    {
        errors |= PositionErrorKind::TOO_MANY_KINGS;
    }

    if let Some(their_king) = pos.board().king_of(!pos.turn()) {
        if pos.king_attackers(their_king, pos.turn(), pos.board().occupied()).any() {
            errors |= PositionErrorKind::OPPOSITE_CHECK;
        }
    }

    if let Some(our_king) = pos.board().king_of(pos.turn()) {
        let checkers = pos.checkers();
        match (checkers.first(), checkers.last()) {
            (Some(a), Some(b)) if a != b && (checkers.count() > 2 || attacks::aligned(a, b, our_king)) => {
                errors |= PositionErrorKind::IMPOSSIBLE_CHECK;
            }
            _ => (),
        }
    }

    errors
}

fn gen_non_king<P: Position>(pos: &P, target: Bitboard, moves: &mut MoveList) {
    gen_pawn_moves(pos, target, moves);
    KnightTag::gen_moves(pos, target, moves);
    BishopTag::gen_moves(pos, target, moves);
    RookTag::gen_moves(pos, target, moves);
    QueenTag::gen_moves(pos, target, moves);
}

fn gen_safe_king<P: Position>(pos: &P, king: Square, target: Bitboard, moves: &mut MoveList) {
    for to in attacks::king_attacks(king) & target {
        if pos.board().attacks_to(to, !pos.turn(), pos.board().occupied()).is_empty() {
            moves.push(Move::Normal {
                role: Role::King,
                from: king,
                capture: pos.board().role_at(to),
                to,
                promotion: None,
            });
        }
    }
}

fn evasions<P: Position>(pos: &P, king: Square, checkers: Bitboard, moves: &mut MoveList) {
    let sliders = checkers & pos.board().sliders();

    let mut attacked = Bitboard(0);
    for checker in sliders {
        attacked |= attacks::ray(checker, king) ^ checker;
    }

    gen_safe_king(pos, king, !pos.us() & !attacked, moves);

    if let Some(checker) = checkers.single_square() {
        let target = attacks::between(king, checker).with(checker);
        gen_non_king(pos, target, moves);
    }
}

fn gen_castling_moves<P: Position>(pos: &P, castles: &Castles, king: Square, side: CastlingSide, moves: &mut MoveList) {
    if let Some(rook) = castles.rook(pos.turn(), side) {
        let path = castles.path(pos.turn(), side);
        if (path & pos.board().occupied()).any() {
            return;
        }

        let king_to = side.king_to(pos.turn());
        let king_path = attacks::between(king, king_to).with(king);
        for sq in king_path {
            if pos.king_attackers(sq, !pos.turn(), pos.board().occupied() ^ king).any() {
                return;
            }
        }

        if pos.king_attackers(king_to, !pos.turn(), pos.board().occupied() ^ king ^ rook ^ side.rook_to(pos.turn())).any() {
            return;
        }

        moves.push(Move::Castle { king, rook });
    }
}

trait Stepper {
    const ROLE: Role;

    fn attacks(from: Square) -> Bitboard;

    fn gen_moves<P: Position>(pos: &P, target: Bitboard, moves: &mut MoveList) {
        for from in pos.our(Self::ROLE) {
            for to in Self::attacks(from) & target {
                moves.push(Move::Normal {
                    role: Self::ROLE,
                    from,
                    capture: pos.board().role_at(to),
                    to,
                    promotion: None,
                });
            }
        }
    }
}

trait Slider {
    const ROLE: Role;
    fn attacks(from: Square, occupied: Bitboard) -> Bitboard;

    fn gen_moves<P: Position>(pos: &P, target: Bitboard, moves: &mut MoveList) {
        for from in pos.our(Self::ROLE) {
            for to in Self::attacks(from, pos.board().occupied()) & target {
                moves.push(Move::Normal {
                    role: Self::ROLE,
                    from,
                    capture: pos.board().role_at(to),
                    to,
                    promotion: None,
                });
            }
        }
    }
}

enum KnightTag { }
enum BishopTag { }
enum RookTag { }
enum QueenTag { }
enum KingTag { }

impl Stepper for KnightTag {
    const ROLE: Role = Role::Knight;
    fn attacks(from: Square) -> Bitboard {
        attacks::knight_attacks(from)
    }
}

impl Stepper for KingTag {
    const ROLE: Role = Role::King;
    fn attacks(from: Square) -> Bitboard {
        attacks::king_attacks(from)
    }
}

impl Slider for BishopTag {
    const ROLE: Role = Role::Bishop;
    fn attacks(from: Square, occupied: Bitboard) -> Bitboard {
        attacks::bishop_attacks(from, occupied)
    }
}

impl Slider for RookTag {
    const ROLE: Role = Role::Rook;
    fn attacks(from: Square, occupied: Bitboard) -> Bitboard {
        attacks::rook_attacks(from, occupied)
    }
}

impl Slider for QueenTag {
    const ROLE: Role = Role::Queen;
    fn attacks(from: Square, occupied: Bitboard) -> Bitboard {
        attacks::queen_attacks(from, occupied)
    }
}

fn gen_pawn_moves<P: Position>(pos: &P, target: Bitboard, moves: &mut MoveList) {
    let seventh = pos.our(Role::Pawn) & Bitboard::relative_rank(pos.turn(), Rank::Seventh);

    for from in pos.our(Role::Pawn) & !seventh {
        for to in attacks::pawn_attacks(pos.turn(), from) & pos.them() & target {
            moves.push(Move::Normal {
                role: Role::Pawn,
                from,
                capture: pos.board().role_at(to),
                to,
                promotion: None,
            });
        }
    }

    for from in seventh {
        for to in attacks::pawn_attacks(pos.turn(), from) & pos.them() & target {
            push_promotions(moves, from, to, pos.board().role_at(to));
        }
    }

    let single_moves = pos.our(Role::Pawn).relative_shift(pos.turn(), 8) &
                       !pos.board().occupied();

    let double_moves = single_moves.relative_shift(pos.turn(), 8) &
                       Bitboard::relative_rank(pos.turn(), Rank::Fourth).with(Bitboard::relative_rank(pos.turn(), Rank::Third)) &
                       !pos.board().occupied();

    for to in single_moves & target & !Bitboard::BACKRANKS {
        if let Some(from) = to.offset(pos.turn().fold(-8, 8)) {
            moves.push(Move::Normal {
                role: Role::Pawn,
                from,
                capture: None,
                to,
                promotion: None,
            });
        }
    }

    for to in single_moves & target & Bitboard::BACKRANKS {
        if let Some(from) = to.offset(pos.turn().fold(-8, 8)) {
            push_promotions(moves, from, to, None);
        }
    }

    for to in double_moves & target {
        if let Some(from) = to.offset(pos.turn().fold(-16, 16)) {
            moves.push(Move::Normal {
                role: Role::Pawn,
                from,
                capture: None,
                to,
                promotion: None,
            });
        }
    }
}

fn push_promotions(moves: &mut MoveList, from: Square, to: Square, capture: Option<Role>) {
    moves.push(Move::Normal { role: Role::Pawn, from, capture, to, promotion: Some(Role::Queen) });
    moves.push(Move::Normal { role: Role::Pawn, from, capture, to, promotion: Some(Role::Rook) });
    moves.push(Move::Normal { role: Role::Pawn, from, capture, to, promotion: Some(Role::Bishop) });
    moves.push(Move::Normal { role: Role::Pawn, from, capture, to, promotion: Some(Role::Knight) });
}

fn add_king_promotions(moves: &mut MoveList) {
    let mut king_promotions = MoveList::new();

    for m in &moves[..] {
        if let Move::Normal { role, from, capture, to, promotion: Some(Role::Queen) } = *m {
            king_promotions.push(Move::Normal {
                role,
                from,
                capture,
                to,
                promotion: Some(Role::King),
            });
        }
    }

    moves.extend(king_promotions);
}

fn has_relevant_ep<P: Position>(pos: &P) -> bool {
    let mut moves = MoveList::new();
    pos.en_passant_moves(&mut moves);
    !moves.is_empty()
}

fn gen_en_passant(board: &Board, turn: Color, ep_square: Option<Square>, moves: &mut MoveList) -> bool {
    let mut found = false;

    if let Some(to) = ep_square {
        for from in board.pawns() & board.by_color(turn) & attacks::pawn_attacks(!turn, to) {
            moves.push(Move::EnPassant { from, to });
            found = true;
        }
    }

    found
}

fn slider_blockers(board: &Board, enemy: Bitboard, king: Square) -> Bitboard {
    let snipers = (attacks::rook_attacks(king, Bitboard(0)) & board.rooks_and_queens()) |
                  (attacks::bishop_attacks(king, Bitboard(0)) & board.bishops_and_queens());

    let mut blockers = Bitboard(0);

    for sniper in snipers & enemy {
        let b = attacks::between(king, sniper) & board.occupied();

        if !b.more_than_one() {
            blockers.add(b);
        }
    }

    blockers
}

fn is_safe<P: Position>(pos: &P, king: Square, m: &Move, blockers: Bitboard) -> bool {
    match *m {
        Move::Normal { from, to, .. } =>
            !blockers.contains(from) || attacks::aligned(from, to, king),
        Move::EnPassant { from, to } => {
            let mut occupied = pos.board().occupied();
            occupied.toggle(from);
            occupied.toggle(Square::from_coords(to.file(), from.rank())); // captured pawn
            occupied.add(to);

            (attacks::rook_attacks(king, occupied) & pos.them() & pos.board().rooks_and_queens()).is_empty() &&
            (attacks::bishop_attacks(king, occupied) & pos.them() & pos.board().bishops_and_queens()).is_empty()
        },
        _ => true,
    }
}

fn filter_san_candidates(role: Role, to: Square, moves: &mut MoveList) {
    moves.retain(|m| match *m {
        Move::Normal { role: r, to: t, .. } | Move::Put { role: r, to: t } =>
            to == t && role == r,
        Move::EnPassant { to: t, .. } => role == Role::Pawn && t == to,
        Move::Castle { .. } => false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fen::Fen;

    struct _AssertObjectSafe(Box<dyn Position>);

    #[test]
    fn test_most_known_legals() {
        let fen = "R6R/3Q4/1Q4Q1/4Q3/2Q4Q/Q4Q2/pp1Q4/kBNN1KB1 w - - 0 1";
        let pos: Chess = fen.parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("legal position");

        let mut moves = MoveList::new();
        pos.legal_moves(&mut moves);
        assert_eq!(moves.len(), 218);
    }

    #[test]
    fn test_pinned_san_candidate() {
        let fen = "R2r2k1/6pp/1Np2p2/1p2pP2/4p3/4K3/3r2PP/8 b - - 5 37";
        let pos: Chess = fen.parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");

        let mut moves = MoveList::new();
        pos.san_candidates(Role::Rook, Square::D3, &mut moves);

        assert_eq!(moves[0], Move::Normal {
            role: Role::Rook,
            from: Square::D2,
            capture: None,
            to: Square::D3,
            promotion: None,
        });

        assert_eq!(moves.len(), 1);
    }

    #[test]
    fn test_promotion() {
        let fen = "3r3K/6PP/8/8/8/2k5/8/8 w - - 0 1";
        let pos: Chess = fen.parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");

        let mut moves = MoveList::new();
        pos.legal_moves(&mut moves);
        assert!(moves.iter().all(|m| m.role() == Role::Pawn));
        assert!(moves.iter().all(|m| m.is_promotion()));
    }

    fn assert_insufficient_material<P>(fen: &str, white: bool, black: bool)
    where
        P: Position + FromSetup,
    {
        let pos: P = fen.parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");

        assert_eq!(pos.has_insufficient_material(White), white);
        assert_eq!(pos.has_insufficient_material(Black), black);
    }

    #[test]
    fn test_insufficient_material() {
        let false_negative = false;

        assert_insufficient_material::<Chess>("8/5k2/8/8/8/8/3K4/8 w - - 0 1", true, true);
        assert_insufficient_material::<Chess>("8/3k4/8/8/2N5/8/3K4/8 b - - 0 1", true, true);
        assert_insufficient_material::<Chess>("8/4rk2/8/8/8/8/3K4/8 w - - 0 1", true, false);
        assert_insufficient_material::<Chess>("8/4qk2/8/8/8/8/3K4/8 w - - 0 1", true, false);
        assert_insufficient_material::<Chess>("8/4bk2/8/8/8/8/3KB3/8 w - - 0 1", false, false);
        assert_insufficient_material::<Chess>("8/8/3Q4/2bK4/B7/8/1k6/8 w - - 1 68", false, false);
        assert_insufficient_material::<Chess>("8/5k2/8/8/8/4B3/3K1B2/8 w - - 0 1", true, true);
        assert_insufficient_material::<Chess>("5K2/8/8/1B6/8/k7/6b1/8 w - - 0 39", true, true);
        assert_insufficient_material::<Chess>("8/8/8/4k3/5b2/3K4/8/2B5 w - - 0 33", true, true);
        assert_insufficient_material::<Chess>("3b4/8/8/6b1/8/8/R7/K1k5 w - - 0 1", false, true);

        assert_insufficient_material::<Atomic>("8/3k4/8/8/2N5/8/3K4/8 b - - 0 1", true, true);
        assert_insufficient_material::<Atomic>("8/4rk2/8/8/8/8/3K4/8 w - - 0 1", true, true);
        assert_insufficient_material::<Atomic>("8/4qk2/8/8/8/8/3K4/8 w - - 0 1", true, false);
        assert_insufficient_material::<Atomic>("8/1k6/8/2n5/8/3NK3/8/8 b - - 0 1", false, false);
        assert_insufficient_material::<Atomic>("8/4bk2/8/8/8/8/3KB3/8 w - - 0 1", true, true);
        assert_insufficient_material::<Atomic>("4b3/5k2/8/8/8/8/3KB3/8 w - - 0 1", false, false);
        assert_insufficient_material::<Atomic>("3Q4/5kKB/8/8/8/8/8/8 b - - 0 1", false, true);
        assert_insufficient_material::<Atomic>("8/5k2/8/8/8/8/5K2/4bb2 w - - 0 1", true, false);
        assert_insufficient_material::<Atomic>("8/5k2/8/8/8/8/5K2/4nb2 w - - 0 1", true, false);

        assert_insufficient_material::<Antichess>("8/4bk2/8/8/8/8/3KB3/8 w - - 0 1", false, false);
        assert_insufficient_material::<Antichess>("4b3/5k2/8/8/8/8/3KB3/8 w - - 0 1", false, false);
        assert_insufficient_material::<Antichess>("8/8/8/6b1/8/3B4/4B3/5B2 w - - 0 1", true, true);
        assert_insufficient_material::<Antichess>("8/8/5b2/8/8/3B4/3B4/8 w - - 0 1", true, false);
        assert_insufficient_material::<Antichess>("8/5p2/5P2/8/3B4/1bB5/8/8 b - - 0 1", false_negative, false_negative);

        assert_insufficient_material::<KingOfTheHill>("8/5k2/8/8/8/8/3K4/8 w - - 0 1", false, false);

        assert_insufficient_material::<RacingKings>("8/5k2/8/8/8/8/3K4/8 w - - 0 1", false, false);

        assert_insufficient_material::<ThreeCheck>("8/5k2/8/8/8/8/3K4/8 w - - 0 1", true, true);
        assert_insufficient_material::<ThreeCheck>("8/5k2/8/8/8/8/3K2N1/8 w - - 0 1", false, true);

        assert_insufficient_material::<Crazyhouse>("8/5k2/8/8/8/8/3K2N1/8 w - - 0 1", true, true);
        assert_insufficient_material::<Crazyhouse>("8/5k2/8/8/8/5B2/3KB3/8 w - - 0 1", false, false);
        assert_insufficient_material::<Crazyhouse>("8/8/8/8/3k4/3N~4/3K4/8 w - - 0 1", false, false);

        assert_insufficient_material::<Horde>("8/5k2/8/8/8/4NN2/8/8 w - - 0 1", false_negative, false);
    }

    #[test]
    fn test_exploded_king_loses_castling_rights() {
        let pos: Atomic = "rnb1kbnr/pppppppp/8/4q3/8/8/PPPPPPPP/RNBQKBNR b KQkq - 0 1".parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");

        let pos = pos.play(&Move::Normal {
            role: Role::Queen,
            from: Square::E5,
            to: Square::E2,
            capture: Some(Role::Pawn),
            promotion: None,
        }).expect("Qxe2# is legal");

        assert_eq!(pos.castling_rights(), Bitboard::from(Square::A8) | Bitboard::from(Square::H8));
        assert_eq!(pos.castles().rook(Color::White, CastlingSide::QueenSide), None);
        assert_eq!(pos.castles().rook(Color::White, CastlingSide::KingSide), None);
        assert_eq!(pos.castles().rook(Color::Black, CastlingSide::QueenSide), Some(Square::A8));
        assert_eq!(pos.castles().rook(Color::Black, CastlingSide::KingSide), Some(Square::H8));
    }

    #[test]
    fn test_racing_kings_end() {
        // Both players reached the backrank.
        let pos: RacingKings = "kr3NK1/1q2R3/8/8/8/5n2/2N5/1rb2B1R w - - 11 14".parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");
        assert!(pos.is_variant_end());
        assert_eq!(pos.variant_outcome(), Some(Outcome::Draw));

        // White to move is lost because black reached the backrank.
        let pos: RacingKings = "1k6/6K1/8/8/8/8/8/8 w - - 0 1".parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");
        assert!(pos.is_variant_end());
        assert_eq!(pos.variant_outcome(), Some(Outcome::Decisive { winner: Color::Black }));

        // Black is given a chance to catch up.
        let pos: RacingKings = "1K6/7k/8/8/8/8/8/8 b - - 0 1".parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");
        assert!(!pos.is_variant_end());
        assert_eq!(pos.variant_outcome(), None);

        // Black near backrank but cannot move there.
        let pos: RacingKings = "2KR4/k7/2Q5/4q3/8/8/8/2N5 b - - 0 1".parse::<Fen>()
            .expect("valid fen")
            .position()
            .expect("valid position");
        assert!(pos.is_variant_end());
        assert_eq!(pos.variant_outcome(), Some(Outcome::Decisive { winner: Color::White }));
    }

    #[test]
    fn test_aligned_checkers() {
        let res = "2Nq4/2K5/1b6/8/7R/3k4/7P/8 w - - 0 1".parse::<Fen>()
            .expect("valid fen")
            .position::<Chess>();
        assert_eq!(res.expect_err("impossible check").kind(), PositionErrorKind::IMPOSSIBLE_CHECK);
    }
}
