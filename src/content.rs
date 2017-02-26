use reader::PdfReader;
use object::*;
use std::fmt::{Display, Formatter};
use std;
use err::*;
use std::mem::swap;
use reader::lexer::Lexer;

#[derive(Debug, Clone)]
pub struct Operation {
	pub operator: String,
	pub operands: Vec<Object>,
}

impl Operation {
	pub fn new(operator: String, operands: Vec<Object>) -> Operation {
		Operation{
			operator: operator,
			operands: operands,
		}
	}
}


#[derive(Debug)]
pub struct Content {
    pub operations: Vec<Operation>,
}

impl PdfReader {
    // TODO it would be optimal to let this be a static method of `Content`, but it
    // requires parsing an object. The reason that is a dynamic method of `PdfReader` is because it
    // needs dereferencing in case of Stream object. However, I don't think a Content Stream should
    // contain that..
    pub fn parse_content(&self, data: &[u8]) -> Result<Content> {
        let mut lexer = Lexer::new(data);

        let mut content = Content {operations: Vec::new()};
        let mut buffer = Vec::new();

        loop {
            let backup_pos = lexer.get_pos();
            let obj = self.parse_object(&mut lexer);
            match obj {
                Ok(obj) => {
                    // Operand
                    buffer.push(obj)
                }
                Err(e) => {
                    // It's not an object/operand - treat it as an operator.
                    lexer.set_pos(backup_pos);
                    let operator = lexer.next()?.as_string(); // TODO will this work as expected?
                    let mut operation = Operation::new(operator, Vec::new());
                    // Give operands to operation and empty buffer.
                    swap(&mut buffer, &mut operation.operands);
                    content.operations.push(operation.clone());
                }
            }
            if lexer.get_pos() > data.len() {
                bail!("Read past boundary of given contents.");
            } else if lexer.get_pos() == data.len() {
                break;
            }
        }
        Ok(content)
    }
}

impl Display for Content {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "Content: ")?;
        for ref operation in &self.operations {
            write!(f, "{}", operation)?;
        }
        Ok(())
    }
}

impl Display for Operation {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "Operation: {} (", self.operator)?;
        for ref operand in &self.operands {
            write!(f, "{}, ", operand)?;
        }
        write!(f, ")\n")
    }
}