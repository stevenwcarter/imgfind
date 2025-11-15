#[derive(Debug, Clone, Copy)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}
// Use so it's easier to read the enums later on
use FocusDirection::*;

use crate::tui::app::App;

impl App {
    /// Handle focus movement in the image grid
    /// Focus wraps to the next/previous row when moving up/down
    pub fn handle_focus(&mut self, direction: FocusDirection) {
        let images_len = self.images.len() as u8;
        let current_index = self.focused_image_index;

        self.focused_image_index = calculate_new_focus_index(images_len, current_index, direction);
    }
}

pub fn calculate_new_focus_index(
    images_len: u8,
    current_index: u8,
    direction: FocusDirection,
) -> u8 {
    let mut current_index = current_index;
    match direction {
        Left => {
            if current_index == 0 {
                current_index = images_len - 1;
            } else {
                current_index -= 1;
            }
        }
        Right => {
            current_index += 1;
            current_index %= images_len;
        }
        Up => {
            if current_index < 3 {
                let remainder = current_index % 3;
                let rows = (images_len - 1) / 3;
                current_index = (rows * 3) + remainder;
                if current_index >= images_len - 1 {
                    current_index = images_len - 1;
                }
            } else {
                current_index -= 3;
            }
        }
        Down => {
            if current_index + 3 >= images_len {
                current_index %= 3
            } else {
                let new_index = current_index + 3;
                if new_index >= images_len {
                    current_index = images_len - 1;
                } else {
                    current_index = new_index;
                }
            }
        }
    }

    current_index
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn it_calculates_correct_right_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Right), 1);
        assert_eq!(calculate_new_focus_index(9, 8, Right), 0);
    }
    #[test]
    fn it_calculates_correct_left_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Left), 8);
        assert_eq!(calculate_new_focus_index(9, 8, Left), 7);
    }
    #[test]
    fn it_calculates_correct_up_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Up), 6);
        assert_eq!(calculate_new_focus_index(9, 1, Up), 7);
        assert_eq!(calculate_new_focus_index(9, 2, Up), 8);
        assert_eq!(calculate_new_focus_index(9, 3, Up), 0);
        assert_eq!(calculate_new_focus_index(9, 4, Up), 1);
        assert_eq!(calculate_new_focus_index(9, 5, Up), 2);
        assert_eq!(calculate_new_focus_index(9, 6, Up), 3);
        assert_eq!(calculate_new_focus_index(9, 7, Up), 4);
        assert_eq!(calculate_new_focus_index(9, 8, Up), 5);
        assert_eq!(calculate_new_focus_index(6, 0, Up), 3);
        assert_eq!(calculate_new_focus_index(6, 1, Up), 4);
        assert_eq!(calculate_new_focus_index(6, 2, Up), 5);
        assert_eq!(calculate_new_focus_index(8, 0, Up), 6);
        assert_eq!(calculate_new_focus_index(8, 1, Up), 7);
        assert_eq!(calculate_new_focus_index(8, 2, Up), 7);
        assert_eq!(calculate_new_focus_index(7, 0, Up), 6);
        assert_eq!(calculate_new_focus_index(7, 1, Up), 6);
        assert_eq!(calculate_new_focus_index(7, 2, Up), 6);
    }
    #[test]
    fn it_calculates_correct_down_indexes() {
        assert_eq!(calculate_new_focus_index(9, 0, Down), 3);
        assert_eq!(calculate_new_focus_index(9, 1, Down), 4);
        assert_eq!(calculate_new_focus_index(9, 2, Down), 5);
        assert_eq!(calculate_new_focus_index(9, 3, Down), 6);
        assert_eq!(calculate_new_focus_index(9, 4, Down), 7);
        assert_eq!(calculate_new_focus_index(9, 5, Down), 8);
        assert_eq!(calculate_new_focus_index(9, 6, Down), 0);
        assert_eq!(calculate_new_focus_index(9, 7, Down), 1);
        assert_eq!(calculate_new_focus_index(9, 8, Down), 2);
        assert_eq!(calculate_new_focus_index(6, 0, Down), 3);
        assert_eq!(calculate_new_focus_index(6, 1, Down), 4);
        assert_eq!(calculate_new_focus_index(6, 2, Down), 5);
        assert_eq!(calculate_new_focus_index(6, 3, Down), 0);
        assert_eq!(calculate_new_focus_index(6, 4, Down), 1);
        assert_eq!(calculate_new_focus_index(6, 5, Down), 2);
        assert_eq!(calculate_new_focus_index(8, 6, Down), 0);
        assert_eq!(calculate_new_focus_index(8, 7, Down), 1);
    }
}
