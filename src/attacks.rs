use square;
use square::Square;
use bitboard::Bitboard;
use types::{ Color, Role, Piece };

const ROOK_DELTAS: [i8; 4] = [8, 1, -8, -1];
const BISHOP_DELTAS: [i8; 4] = [9, 7, -9, -7];
const KING_DELTAS: [i8; 8] = [9, 8, 7, 1, -9, -8, -7, -1];
const KNIGHT_DELTAS: [i8; 8] = [17, 15, 10, 6, -17, -15, -10, -6];
const WHITE_PAWN_DELTAS: [i8; 2] = [7, 9];
const BLACK_PAWN_DELTAS: [i8; 2] = [-7, -9];

fn sliding_attacks(sq: Square, occupied: Bitboard, deltas: &[i8]) -> Bitboard {
    let mut attack = Bitboard(0);

    for delta in deltas {
        let Square(mut s) = sq;

        loop {
            s += *delta;
            if s < 0 || s >= 64 || square::distance(Square(s), Square(s - delta)) > 2 {
                break;
            }

            attack.add(Square(s));

            if occupied.contains(Square(s)) {
                break;
            }
        }
    }

    attack
}

fn step_attacks(sq: Square, deltas: &[i8]) -> Bitboard {
    sliding_attacks(sq, Bitboard::all(), deltas)
}

fn init_magics(indexes: &mut[usize], masks: &mut[Bitboard], ranges: &mut[Bitboard], attacks: &mut Vec<u16>, deltas: &[i8]) {
    for s in 0..64 {
        let sq = Square(s);

        let range = sliding_attacks(sq, Bitboard(0), deltas);
        ranges[s as usize] = range;

        let edges = ((Bitboard::rank(0) | Bitboard::rank(7)) & !Bitboard::rank(sq.rank())) |
                    ((Bitboard::file(0) | Bitboard::file(7)) & !Bitboard::file(sq.file()));

        let mask = range & !edges;
        masks[s as usize] = mask;

        let mut size = 0;
        for subset in mask.subsets() {
            let index = indexes[s as usize] + subset.extract(mask) as usize;
            attacks[index] = sliding_attacks(sq, subset, deltas).extract(range) as u16;
            size += 1;
        }

        if s < 63 {
            indexes[s as usize + 1] = indexes[s as usize] + size;
        }
    }
}

pub struct Precomp {
    knight_attacks: [Bitboard; 64],
    king_attacks: [Bitboard; 64],
    white_pawn_attacks: [Bitboard; 64],
    black_pawn_attacks: [Bitboard; 64],

    rook_indexes: [usize; 64],
    rook_masks: [Bitboard; 64],
    rook_ranges: [Bitboard; 64],
    rook_attacks: Vec<u16>,

    bishop_indexes: [usize; 64],
    bishop_masks: [Bitboard; 64],
    bishop_ranges: [Bitboard; 64],
    bishop_attacks: Vec<u16>,

    bb_rays: [[Bitboard; 64]; 64],
    bb_between: [[Bitboard; 64]; 64],
}

impl Precomp {
    pub fn new() -> Precomp {
        let mut precomp = Precomp {
            knight_attacks: [Bitboard(0); 64],
            king_attacks: [Bitboard(0); 64],
            white_pawn_attacks: [Bitboard(0); 64],
            black_pawn_attacks: [Bitboard(0); 64],

            rook_indexes: [0; 64],
            rook_masks: [Bitboard(0); 64],
            rook_ranges: [Bitboard(0); 64],
            rook_attacks: vec![0; 0x19000],

            bishop_indexes: [0; 64],
            bishop_masks: [Bitboard(0); 64],
            bishop_ranges: [Bitboard(0); 64],
            bishop_attacks: vec![0; 0x1480],

            bb_rays: [[Bitboard(0); 64]; 64],
            bb_between: [[Bitboard(0); 64]; 64],
        };

        for s in 0..64 {
            precomp.knight_attacks[s] = step_attacks(Square(s as i8), &KNIGHT_DELTAS);
            precomp.king_attacks[s] = step_attacks(Square(s as i8), &KING_DELTAS);
            precomp.white_pawn_attacks[s] = step_attacks(Square(s as i8), &WHITE_PAWN_DELTAS);
            precomp.black_pawn_attacks[s] = step_attacks(Square(s as i8), &BLACK_PAWN_DELTAS);
        }

        init_magics(&mut precomp.rook_indexes,
                    &mut precomp.rook_masks,
                    &mut precomp.rook_ranges,
                    &mut precomp.rook_attacks,
                    &ROOK_DELTAS);

        init_magics(&mut precomp.bishop_indexes,
                    &mut precomp.bishop_masks,
                    &mut precomp.bishop_ranges,
                    &mut precomp.bishop_attacks,
                    &BISHOP_DELTAS);

        for a in 0..64 {
            let sa = Square(a as i8);

            for b in 0..64 {
                let sb = Square(b as i8);

                if precomp.bishop_attacks(sa, Bitboard(0)).contains(sb) {
                    precomp.bb_rays[a][b] =
                        (precomp.bishop_attacks(sa, Bitboard(0)) &
                         precomp.bishop_attacks(sb, Bitboard(0))).with(sa).with(sb);
                    precomp.bb_between[a][b] =
                        precomp.bishop_attacks(sa, Bitboard::from_square(sb)) &
                        precomp.bishop_attacks(sb, Bitboard::from_square(sa));
                } else if precomp.rook_attacks(sa, Bitboard(0)).contains(sb) {
                    precomp.bb_rays[a][b] =
                        (precomp.rook_attacks(sa, Bitboard(0)) &
                         precomp.rook_attacks(sb, Bitboard(0))).with(sa).with(sb);
                    precomp.bb_between[a][b] =
                        precomp.rook_attacks(sa, Bitboard::from_square(sb)) &
                        precomp.rook_attacks(sb, Bitboard::from_square(sa));
                }
            }
        }

        precomp
    }

    pub fn pawn_attacks(&self, color: Color, Square(sq): Square) -> Bitboard {
        let table = match color {
            Color::White => self.white_pawn_attacks,
            Color::Black => self.black_pawn_attacks,
        };

        table[sq as usize]
    }

    pub fn knight_attacks(&self, Square(sq): Square) -> Bitboard {
        self.knight_attacks[sq as usize]
    }

    pub fn king_attacks(&self, Square(sq): Square) -> Bitboard {
        self.king_attacks[sq as usize]
    }

    pub fn rook_attacks(&self, Square(sq): Square, occupied: Bitboard) -> Bitboard {
        let mask = self.rook_masks[sq as usize];
        let range = self.rook_ranges[sq as usize];
        Bitboard::deposit(self.rook_attacks[self.rook_indexes[sq as usize] + occupied.extract(mask) as usize] as u64, range)
    }

    pub fn bishop_attacks(&self, Square(sq): Square, occupied: Bitboard) -> Bitboard {
        let mask = self.bishop_masks[sq as usize];
        let range = self.bishop_ranges[sq as usize];
        Bitboard::deposit(self.bishop_attacks[self.bishop_indexes[sq as usize] + occupied.extract(mask) as usize] as u64, range)
    }

    pub fn queen_attacks(&self, sq: Square, occupied: Bitboard) -> Bitboard {
        self.rook_attacks(sq, occupied) | self.bishop_attacks(sq, occupied)
    }

    pub fn attacks(&self, sq: Square, Piece { color, role }: Piece, occupied: Bitboard) -> Bitboard {
        match role {
            Role::Pawn   => self.pawn_attacks(color, sq),
            Role::Knight => self.knight_attacks(sq),
            Role::Bishop => self.bishop_attacks(sq, occupied),
            Role::Rook   => self.rook_attacks(sq, occupied),
            Role::Queen  => self.queen_attacks(sq, occupied),
            Role::King   => self.king_attacks(sq)
        }
    }

    pub fn ray(&self, Square(a): Square, Square(b): Square) -> Bitboard {
        self.bb_rays[a as usize][b as usize]
    }

    pub fn between(&self, Square(a): Square, Square(b): Square) -> Bitboard {
        self.bb_between[a as usize][b as usize]
    }

    pub fn aligned(&self, a: Square, b: Square, c: Square) -> bool {
        self.ray(a, b).contains(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sliding_rook_attacks() {
        let attack = sliding_attacks(square::D6, Bitboard(0x3f7f28802826f5b9), &ROOK_DELTAS);
        assert_eq!(attack, Bitboard(0x8370808000000));
    }

    #[test]
    fn test_rook_attacks() {
        let table = Precomp::new();
        assert_eq!(table.rook_attacks(square::D6, Bitboard(0x3f7f28802826f5b9)), Bitboard(0x8370808000000));
    }
}
